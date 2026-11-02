//! Maps to: CC `tools/PowerShellTool/modeValidation.ts`.
//!
//! Checks whether commands should be auto-allowed based on the current
//! permission mode. In acceptEdits mode, filesystem-modifying PowerShell
//! cmdlets are auto-allowed. Follows the same patterns as BashTool mode
//! validation.
//!
//! This module only ever returns `allow` or `passthrough`: it is an
//! auto-approval fast path, never a blocking one. Every guard below therefore
//! errs toward `passthrough`, and an AST that could not be produced (a failed
//! or unavailable parse) falls straight through to the normal ask flow.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use crate::utils::powershell::parser::{
    CommandNameType, PS_TOKENIZER_DASH_CHARS, ParsedCommandElement, ParsedPowerShellCommand,
    PipelineElementType, derive_security_flags, get_pipeline_segments,
};

use super::read_only_validation::{
    arg_leaks_value, is_allowlisted_pipeline_tail, is_cwd_changing_cmdlet, is_safe_output_command,
    resolve_to_canonical,
};

/// Maps to: CC `modeValidation.ts:33-38#ACCEPT_EDITS_ALLOWED_CMDLETS`.
///
/// Tier 3 cmdlets with complex parameter binding are deliberately absent — they
/// fall through to ask. Only simple write cmdlets whose first positional is
/// `-Path` are auto-allowed here, and those get path validation through
/// `CMDLET_PATH_CONFIG`.
const ACCEPT_EDITS_ALLOWED_CMDLETS: [&str; 4] =
    ["set-content", "add-content", "remove-item", "clear-content"];

/// Maps to: CC `modeValidation.ts:40-47#isAcceptEditsAllowedCmdlet`.
fn is_accept_edits_allowed_cmdlet(name: &str) -> bool {
    ACCEPT_EDITS_ALLOWED_CMDLETS.contains(&resolve_to_canonical(name).as_str())
}

/// Maps to: CC `modeValidation.ts:56#LINK_ITEM_TYPES`.
///
/// All three redirect path resolution at runtime: symbolic links and junctions
/// are reparse points, and hard links alias a file's inode. Any of them lets a
/// later relative-path write land outside the validator's view.
const LINK_ITEM_TYPES: [&str; 3] = ["symboliclink", "junction", "hardlink"];

/// Maps to: CC `modeValidation.ts:64-69#isItemTypeParamAbbrev`.
///
/// Minimum prefixes are `-it` (avoiding ambiguity with other New-Item
/// parameters) and `-ty` (avoiding a `-t` collision with `-Target`).
fn is_item_type_param_abbrev(param: &str) -> bool {
    (param.len() >= 3 && "-itemtype".starts_with(param))
        || (param.len() >= 3 && "-type".starts_with(param))
}

/// Maps to: CC `modeValidation.ts:82-117#isSymlinkCreatingCommand`.
///
/// Handles PowerShell parameter abbreviation (`-it` through `-itemtype`),
/// Unicode dash prefixes, and colon-bound values (`-it:Junction`).
pub fn is_symlink_creating_command(cmd: &ParsedCommandElement) -> bool {
    if resolve_to_canonical(&cmd.name) != "new-item" {
        return false;
    }
    for (index, raw) in cmd.args.iter().enumerate() {
        let Some(first) = raw.chars().next() else {
            continue;
        };
        // PowerShell's tokenizer treats all four dash characters plus `/` (the
        // 5.1 parameter prefix) as parameter markers, so normalize to ASCII `-`
        // before prefix comparison.
        let normalized = if PS_TOKENIZER_DASH_CHARS.contains(&first) || first == '/' {
            format!("-{}", raw.chars().skip(1).collect::<String>())
        } else {
            raw.clone()
        };
        let lower = normalized.to_lowercase();
        let colon_index = lower
            .char_indices()
            .skip(1)
            .find_map(|(position, character)| (character == ':').then_some(position));
        let param_raw = match colon_index {
            Some(position) => &lower[..position],
            None => lower.as_str(),
        };
        // Strip backtick escapes: `-Item`Type` resolves to `-ItemType`.
        let param = param_raw.replace('`', "");
        if !is_item_type_param_abbrev(&param) {
            continue;
        }
        let raw_value = match colon_index {
            Some(position) => lower[position + 1..].to_string(),
            None => cmd
                .args
                .get(index + 1)
                .map(|value| value.to_lowercase())
                .unwrap_or_default(),
        };
        // Space-separated args arrive backtick-resolved from the .NET parser,
        // but colon-bound values use the raw source text.
        let value = raw_value.replace('`', "");
        let value = value.strip_prefix(['\'', '"']).unwrap_or(&value);
        let value = value.strip_suffix(['\'', '"']).unwrap_or(value);
        if LINK_ITEM_TYPES.contains(&value) {
            return true;
        }
    }
    false
}

fn passthrough(message: &str) -> PermissionResult {
    PermissionResult::Passthrough {
        message: message.to_string(),
        decision_reason: None,
        suggestions: Vec::new(),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

/// Maps to: CC `modeValidation.ts:132-404#checkPermissionMode`.
///
/// Returns `allow` when the current mode permits auto-approval, and
/// `passthrough` when no mode-specific handling applies.
pub fn check_permission_mode(
    command: &str,
    parsed: &ParsedPowerShellCommand,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    // Skip bypass and dontAsk modes (handled in the main permission flow).
    if matches!(
        tool_permission_context.mode,
        PermissionMode::BypassPermissions | PermissionMode::DontAsk
    ) {
        return passthrough("Mode is handled in main permission flow");
    }

    if tool_permission_context.mode != PermissionMode::AcceptEdits {
        return passthrough("No mode-specific validation required");
    }

    if !parsed.valid {
        return passthrough("Cannot validate mode for unparsed command");
    }

    // SECURITY: subexpressions, script blocks, and member invocations could
    // smuggle arbitrary code through acceptEdits mode.
    let security_flags = derive_security_flags(parsed);
    if security_flags.has_sub_expressions
        || security_flags.has_script_blocks
        || security_flags.has_member_invocations
        || security_flags.has_splatting
        || security_flags.has_assignments
        || security_flags.has_stop_parsing
        || security_flags.has_expandable_strings
    {
        return passthrough(
            "Command contains subexpressions, script blocks, or member invocations that require approval",
        );
    }

    let segments = get_pipeline_segments(parsed);

    // SECURITY: a valid parse with no segments means there are no commands to
    // check, which is not a reason to auto-allow.
    if segments.is_empty() {
        return passthrough("No commands found to validate for acceptEdits mode");
    }

    // SECURITY: compound cwd desync guard — BashTool parity. When any statement
    // contains Set-Location/Push-Location/Pop-Location, the cwd changes between
    // statements while path validation resolves relative paths against the
    // stale process cwd, so a later write cmdlet targets a different directory
    // than the validator checked.
    let total_commands: usize = segments.iter().map(|segment| segment.commands.len()).sum();
    if total_commands > 1 {
        let mut has_cd_command = false;
        let mut has_symlink_create = false;
        let mut has_write_command = false;
        for segment in segments {
            for cmd in &segment.commands {
                if cmd.element_type != Some(PipelineElementType::CommandAst) {
                    continue;
                }
                if is_cwd_changing_cmdlet(&cmd.name) {
                    has_cd_command = true;
                }
                if is_symlink_creating_command(cmd) {
                    has_symlink_create = true;
                }
                if is_accept_edits_allowed_cmdlet(&cmd.name) {
                    has_write_command = true;
                }
            }
        }
        if has_cd_command && has_write_command {
            return passthrough(
                "Compound command contains a directory-changing command (Set-Location/Push-Location/Pop-Location) with a write operation — cannot auto-allow because path validation uses stale cwd",
            );
        }
        // SECURITY: link-create compound guard, mirroring the cd guard.
        // `New-Item -ItemType SymbolicLink -Path ./link -Value /etc; Get-Content
        // ./link/passwd` validates ./link/passwd against a cwd with no link,
        // while runtime follows the just-created link. There is no
        // has_write_command requirement here: read-through-symlink exfiltration
        // is equally dangerous.
        if has_symlink_create {
            return passthrough(
                "Compound command creates a filesystem link (New-Item -ItemType SymbolicLink/Junction/HardLink) — cannot auto-allow because path validation cannot follow just-created links",
            );
        }
    }

    for segment in segments {
        for cmd in &segment.commands {
            if cmd.element_type != Some(PipelineElementType::CommandAst) {
                // SECURITY: this guard is load-bearing for three cases and must
                // not be narrowed.
                //
                // 1. Expression pipeline sources: `'/etc/passwd' | Remove-Item`
                //    binds the string literal to -Path, and we cannot statically
                //    know what path it represents.
                // 2. Control-flow statements: `foreach ($x in ...) { Remove-Item
                //    $x }` produces a synthetic CommandExpressionAst entry.
                //    Without this guard, the nested Remove-Item would be checked
                //    below and auto-allowed on an unvalidatable loop variable.
                // 3. Non-PipelineAst redirection coverage: `cmd && cmd2 > /tmp`
                //    also produces a synthetic element here.
                let element_type = cmd
                    .element_type
                    .map(PipelineElementType::as_str)
                    .unwrap_or("CommandExpressionAst");
                return passthrough(&format!(
                    "Pipeline contains expression source ({element_type}) that cannot be statically validated"
                ));
            }
            // SECURITY: nameType is computed from the raw, pre-strip name.
            // `scripts\Remove-Item` strips to `Remove-Item` and would match the
            // acceptEdits allowlist, but PowerShell runs
            // `scripts\Remove-Item.ps1`.
            if cmd.name_type == Some(CommandNameType::Application) {
                return passthrough(&format!(
                    "Command '{}' resolved from a path-like name and requires approval",
                    cmd.name
                ));
            }
            // SECURITY: elementTypes whitelist. `derive_security_flags` above
            // does not flag bare Variable/Other element types, so
            // `Remove-Item $env:PATH` would resolve the literal text as a
            // relative path inside the cwd and auto-allow, while PowerShell
            // expands the variable and deletes the real target.
            if let Some(element_types) = cmd.element_types.as_ref() {
                for (index, element_type) in element_types.iter().enumerate().skip(1) {
                    if *element_type
                        != crate::utils::powershell::parser::CommandElementType::StringConstant
                        && *element_type
                            != crate::utils::powershell::parser::CommandElementType::Parameter
                    {
                        return passthrough(&format!(
                            "Command argument has unvalidatable type ({}) — variable paths cannot be statically resolved",
                            element_type.as_str()
                        ));
                    }
                    if *element_type
                        == crate::utils::powershell::parser::CommandElementType::Parameter
                    {
                        // elementTypes[i] tracks args[i-1]; elementTypes[0] is
                        // the command name. A colon-bound paren such as
                        // `-Path:(1 > /tmp/x)` passes the whitelist but
                        // evaluates at runtime and writes the file.
                        let arg = cmd
                            .args
                            .get(index - 1)
                            .map(String::as_str)
                            .unwrap_or_default();
                        if let Some(colon_index) = arg.find(':')
                            && colon_index > 0
                            && arg[colon_index + 1..].contains(['$', '(', '@', '{', '['])
                        {
                            return passthrough(
                                "Colon-bound parameter contains an expression that cannot be statically validated",
                            );
                        }
                    }
                }
            }
            // Safe output cmdlets and allowlisted pipeline-tail transformers do
            // not affect the semantics of the preceding command, so
            // `Remove-Item ./foo | Out-Null` auto-allows like the bare cmdlet.
            if is_safe_output_command(&cmd.name) || is_allowlisted_pipeline_tail(cmd, command) {
                continue;
            }
            if !is_accept_edits_allowed_cmdlet(&cmd.name) {
                return passthrough(&format!(
                    "No mode-specific handling for '{}' in acceptEdits mode",
                    cmd.name
                ));
            }
            // SECURITY: reject unclassifiable argument types. 'Other' covers
            // HashtableAst, ConvertExpressionAst, and BinaryExpressionAst, all
            // of which can nest redirections or code the parser cannot fully
            // decompose — `@{k='payload' > ~/.bashrc}` as a -Value argument
            // would otherwise pass.
            if arg_leaks_value(Some(cmd)) {
                return passthrough(&format!(
                    "Arguments in '{}' cannot be statically validated in acceptEdits mode",
                    cmd.name
                ));
            }
        }

        // Also check nested commands from control flow statements.
        if let Some(nested_commands) = segment.nested_commands.as_ref() {
            for cmd in nested_commands {
                if cmd.element_type != Some(PipelineElementType::CommandAst) {
                    let element_type = cmd
                        .element_type
                        .map(PipelineElementType::as_str)
                        .unwrap_or("CommandExpressionAst");
                    return passthrough(&format!(
                        "Nested expression element ({element_type}) cannot be statically validated"
                    ));
                }
                if cmd.name_type == Some(CommandNameType::Application) {
                    return passthrough(&format!(
                        "Nested command '{}' resolved from a path-like name and requires approval",
                        cmd.name
                    ));
                }
                if is_safe_output_command(&cmd.name) || is_allowlisted_pipeline_tail(cmd, command) {
                    continue;
                }
                if !is_accept_edits_allowed_cmdlet(&cmd.name) {
                    return passthrough(&format!(
                        "No mode-specific handling for '{}' in acceptEdits mode",
                        cmd.name
                    ));
                }
                if arg_leaks_value(Some(cmd)) {
                    return passthrough(&format!(
                        "Arguments in nested '{}' cannot be statically validated in acceptEdits mode",
                        cmd.name
                    ));
                }
            }
        }
    }

    PermissionResult::Allow {
        updated_input: Some(serde_json::json!({ "command": command })),
        user_modified: None,
        decision_reason: Some(PermissionDecisionReason::Mode {
            mode: PermissionMode::AcceptEdits,
        }),
        tool_use_id: None,
        accept_feedback: None,
        content_blocks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::powershell::parser::{CommandElementType, ParsedStatement, StatementType};

    fn cmd(
        name: &str,
        args: &[&str],
        element_types: &[CommandElementType],
    ) -> ParsedCommandElement {
        let mut types = vec![CommandElementType::StringConstant];
        types.extend_from_slice(element_types);
        ParsedCommandElement {
            name: name.to_string(),
            name_type: Some(CommandNameType::Cmdlet),
            element_type: Some(PipelineElementType::CommandAst),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            text: format!("{name} {}", args.join(" ")).trim_end().to_string(),
            element_types: Some(types),
            children: None,
            redirections: None,
        }
    }

    fn literals(count: usize) -> Vec<CommandElementType> {
        vec![CommandElementType::StringConstant; count]
    }

    fn statement(commands: Vec<ParsedCommandElement>) -> ParsedStatement {
        ParsedStatement {
            statement_type: StatementType::PipelineAst,
            commands,
            redirections: Vec::new(),
            text: String::new(),
            nested_commands: None,
            security_patterns: None,
        }
    }

    fn parsed(statements: Vec<ParsedStatement>) -> ParsedPowerShellCommand {
        ParsedPowerShellCommand {
            valid: true,
            errors: Vec::new(),
            statements,
            variables: Vec::new(),
            has_stop_parsing: false,
            original_command: String::new(),
            type_literals: Vec::new(),
            has_using_statements: false,
            has_script_requirements: false,
        }
    }

    fn accept_edits() -> ToolPermissionContext {
        ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        }
    }

    #[test]
    fn non_accept_edits_modes_never_auto_allow_here() {
        let command = "Set-Content ./a.txt hi";
        let parsed = parsed(vec![statement(vec![cmd(
            "Set-Content",
            &["./a.txt"],
            &literals(1),
        )])]);
        for (mode, message) in [
            (
                PermissionMode::BypassPermissions,
                "Mode is handled in main permission flow",
            ),
            (
                PermissionMode::DontAsk,
                "Mode is handled in main permission flow",
            ),
            (
                PermissionMode::Default,
                "No mode-specific validation required",
            ),
            (PermissionMode::Plan, "No mode-specific validation required"),
        ] {
            let context = ToolPermissionContext {
                mode,
                ..ToolPermissionContext::default()
            };
            assert!(
                matches!(
                    check_permission_mode(command, &parsed, &context),
                    PermissionResult::Passthrough { ref message, .. } if message == message
                ),
                "mode={mode:?} expected {message}"
            );
        }
    }

    #[test]
    fn an_unparsed_command_is_never_auto_allowed() {
        let invalid = ParsedPowerShellCommand {
            valid: false,
            ..parsed(vec![statement(vec![cmd(
                "Set-Content",
                &["./a.txt"],
                &literals(1),
            )])])
        };
        assert!(matches!(
            check_permission_mode("Set-Content ./a.txt", &invalid, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Cannot validate mode for unparsed command"
        ));
    }

    #[test]
    fn a_missing_powershell_parse_falls_through_instead_of_allowing() {
        // The exact shape produced by parser.rs when no PowerShell is installed.
        let unavailable = ParsedPowerShellCommand {
            valid: false,
            errors: vec![crate::utils::powershell::parser::ParseError {
                message: "PowerShell is not available".to_string(),
                error_id: "NoPowerShell".to_string(),
            }],
            ..parsed(Vec::new())
        };
        assert!(matches!(
            check_permission_mode("Remove-Item ./a.txt", &unavailable, &accept_edits()),
            PermissionResult::Passthrough { .. }
        ));
    }

    #[test]
    fn empty_statement_lists_do_not_auto_allow() {
        assert!(matches!(
            check_permission_mode("", &parsed(Vec::new()), &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "No commands found to validate for acceptEdits mode"
        ));
    }

    #[test]
    fn simple_write_cmdlets_auto_allow_in_accept_edits() {
        let parsed = parsed(vec![statement(vec![cmd(
            "Set-Content",
            &["./a.txt", "-Value", "hi"],
            &[
                CommandElementType::StringConstant,
                CommandElementType::Parameter,
                CommandElementType::StringConstant,
            ],
        )])]);
        assert!(matches!(
            check_permission_mode("Set-Content ./a.txt -Value hi", &parsed, &accept_edits()),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Mode {
                    mode: PermissionMode::AcceptEdits
                }),
                ..
            }
        ));
    }

    #[test]
    fn tier_three_write_cmdlets_still_require_approval() {
        for name in ["New-Item", "Copy-Item", "Move-Item", "Out-File"] {
            let parsed = parsed(vec![statement(vec![cmd(name, &["./a.txt"], &literals(1))])]);
            assert!(
                matches!(
                    check_permission_mode("x", &parsed, &accept_edits()),
                    PermissionResult::Passthrough { ref message, .. }
                        if message.starts_with("No mode-specific handling for")
                ),
                "name={name:?}"
            );
        }
    }

    #[test]
    fn safe_output_and_pipeline_tails_do_not_block_the_write() {
        let parsed = parsed(vec![statement(vec![
            cmd("Remove-Item", &["./a.txt"], &literals(1)),
            cmd("Out-Null", &[], &[]),
        ])]);
        assert!(matches!(
            check_permission_mode("Remove-Item ./a.txt | Out-Null", &parsed, &accept_edits()),
            PermissionResult::Allow { .. }
        ));

        let tail = parsed_with_tail();
        assert!(matches!(
            check_permission_mode(
                "Set-Content ./a.txt hi | Format-Table",
                &tail,
                &accept_edits()
            ),
            PermissionResult::Allow { .. }
        ));
    }

    fn parsed_with_tail() -> ParsedPowerShellCommand {
        parsed(vec![statement(vec![
            cmd("Set-Content", &["./a.txt", "hi"], &literals(2)),
            cmd("Format-Table", &[], &[]),
        ])])
    }

    #[test]
    fn security_flag_carriers_are_never_auto_allowed() {
        let mut with_variable = parsed(vec![statement(vec![cmd(
            "Remove-Item",
            &["$env:PATH"],
            &[CommandElementType::Variable],
        )])]);
        assert!(matches!(
            check_permission_mode("Remove-Item $env:PATH", &with_variable, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message.starts_with("Command argument has unvalidatable type (Variable)")
        ));

        with_variable.statements[0].commands[0].element_types = Some(vec![
            CommandElementType::StringConstant,
            CommandElementType::SubExpression,
        ]);
        assert!(matches!(
            check_permission_mode("Remove-Item $(x)", &with_variable, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Command contains subexpressions, script blocks, or member invocations that require approval"
        ));
    }

    #[test]
    fn colon_bound_expressions_block_auto_allow() {
        let parsed = parsed(vec![statement(vec![cmd(
            "Remove-Item",
            &["-Path:(1 > /tmp/x)"],
            &[CommandElementType::Parameter],
        )])]);
        assert!(matches!(
            check_permission_mode("Remove-Item -Path:(1 > /tmp/x)", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Colon-bound parameter contains an expression that cannot be statically validated"
        ));
    }

    #[test]
    fn path_like_command_names_block_auto_allow() {
        let mut spoofed = cmd("Remove-Item", &["./a.txt"], &literals(1));
        spoofed.name_type = Some(CommandNameType::Application);
        let parsed = parsed(vec![statement(vec![spoofed])]);
        assert!(matches!(
            check_permission_mode("scripts\\Remove-Item ./a.txt", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Command 'Remove-Item' resolved from a path-like name and requires approval"
        ));
    }

    #[test]
    fn expression_pipeline_sources_block_auto_allow() {
        let parsed = parsed(vec![statement(vec![
            ParsedCommandElement {
                name: "'/etc/passwd'".to_string(),
                name_type: Some(CommandNameType::Unknown),
                element_type: Some(PipelineElementType::CommandExpressionAst),
                text: "'/etc/passwd'".to_string(),
                ..ParsedCommandElement::default()
            },
            cmd("Remove-Item", &[], &[]),
        ])]);
        assert!(matches!(
            check_permission_mode("'/etc/passwd' | Remove-Item", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Pipeline contains expression source (CommandExpressionAst) that cannot be statically validated"
        ));
    }

    #[test]
    fn compound_cwd_change_with_a_write_blocks_auto_allow() {
        let parsed = parsed(vec![
            statement(vec![cmd("Set-Location", &["./.claude"], &literals(1))]),
            statement(vec![cmd("Set-Content", &["./settings.json"], &literals(1))]),
        ]);
        assert!(matches!(
            check_permission_mode("Set-Location ./.claude; Set-Content ./settings.json", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message.starts_with("Compound command contains a directory-changing command")
        ));
    }

    #[test]
    fn compound_link_creation_blocks_auto_allow_even_without_a_write() {
        let parsed = parsed(vec![
            statement(vec![cmd(
                "New-Item",
                &["-ItemType", "SymbolicLink", "-Path", "./link"],
                &[
                    CommandElementType::Parameter,
                    CommandElementType::StringConstant,
                    CommandElementType::Parameter,
                    CommandElementType::StringConstant,
                ],
            )]),
            statement(vec![cmd("Get-Content", &["./link/passwd"], &literals(1))]),
        ]);
        assert!(matches!(
            check_permission_mode("New-Item -ItemType SymbolicLink -Path ./link; Get-Content ./link/passwd", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message.starts_with("Compound command creates a filesystem link")
        ));
    }

    #[test]
    fn symlink_detection_handles_abbreviations_dashes_and_colon_values() {
        for args in [
            vec!["-ItemType", "SymbolicLink"],
            vec!["-it", "Junction"],
            vec!["-ty", "HardLink"],
            vec!["-it:SymbolicLink"],
            vec!["-it:'Junction'"],
            vec!["\u{2013}ItemType", "symboliclink"],
            vec!["/ItemType", "Junction"],
            vec!["-Item`Type", "SymbolicLink"],
        ] {
            let element_types = vec![CommandElementType::Parameter; args.len()];
            assert!(
                is_symlink_creating_command(&cmd("New-Item", &args, &element_types)),
                "args={args:?}"
            );
        }
        assert!(!is_symlink_creating_command(&cmd(
            "New-Item",
            &["-ItemType", "File"],
            &[
                CommandElementType::Parameter,
                CommandElementType::StringConstant
            ]
        )));
        assert!(!is_symlink_creating_command(&cmd(
            "Set-Content",
            &["-ItemType", "SymbolicLink"],
            &[
                CommandElementType::Parameter,
                CommandElementType::StringConstant
            ]
        )));
    }

    #[test]
    fn nested_control_flow_commands_are_validated_too() {
        let mut parsed = parsed(vec![statement(vec![ParsedCommandElement {
            name: "foreach".to_string(),
            name_type: Some(CommandNameType::Unknown),
            element_type: Some(PipelineElementType::CommandAst),
            text: "foreach".to_string(),
            element_types: Some(vec![CommandElementType::StringConstant]),
            ..ParsedCommandElement::default()
        }])]);
        parsed.statements[0].commands[0].name = "Remove-Item".to_string();
        parsed.statements[0].nested_commands = Some(vec![cmd("Start-Process", &[], &[])]);
        assert!(matches!(
            check_permission_mode("x", &parsed, &accept_edits()),
            PermissionResult::Passthrough { ref message, .. }
                if message == "No mode-specific handling for 'Start-Process' in acceptEdits mode"
        ));
    }
}
