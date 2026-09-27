//! Maps to: CC `tools/BashTool/pathValidation.ts`.
//!
//! Bash-specific path extraction and validation orchestration. The shared path
//! safety rules remain in `utils/permissions/path_validation.rs`, matching the
//! official split between Bash command parsing and generic path policy.
//!
//! Internal tree-sitter builds consume AST-derived `Redirect[]` /
//! `SimpleCommand[]` inputs directly; parser-unavailable external builds retain
//! the official legacy `splitCommand_DEPRECATED` fallback.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    AdditionalWorkingDirectory, PermissionMode, PermissionRuleSource, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::bash::parsed_command::OutputRedirection;
use crate::utils::path::get_directory_for_path;
use crate::utils::permissions::filesystem::all_working_directories;
use crate::utils::permissions::path_validation::{
    FileOperationType, expand_tilde, format_directory_list, is_dangerous_removal_path,
    validate_path,
};
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathCommand {
    Cd,
    Ls,
    Find,
    Mkdir,
    Touch,
    Rm,
    Rmdir,
    Mv,
    Cp,
    Cat,
    Head,
    Tail,
    Sort,
    Uniq,
    Wc,
    Cut,
    Paste,
    Column,
    Tr,
    File,
    Stat,
    Diff,
    Awk,
    Strings,
    Hexdump,
    Od,
    Base64,
    Nl,
    Grep,
    Rg,
    Sed,
    Git,
    Jq,
    Sha256sum,
    Sha1sum,
    Md5sum,
}

impl PathCommand {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "cd" => Self::Cd,
            "ls" => Self::Ls,
            "find" => Self::Find,
            "mkdir" => Self::Mkdir,
            "touch" => Self::Touch,
            "rm" => Self::Rm,
            "rmdir" => Self::Rmdir,
            "mv" => Self::Mv,
            "cp" => Self::Cp,
            "cat" => Self::Cat,
            "head" => Self::Head,
            "tail" => Self::Tail,
            "sort" => Self::Sort,
            "uniq" => Self::Uniq,
            "wc" => Self::Wc,
            "cut" => Self::Cut,
            "paste" => Self::Paste,
            "column" => Self::Column,
            "tr" => Self::Tr,
            "file" => Self::File,
            "stat" => Self::Stat,
            "diff" => Self::Diff,
            "awk" => Self::Awk,
            "strings" => Self::Strings,
            "hexdump" => Self::Hexdump,
            "od" => Self::Od,
            "base64" => Self::Base64,
            "nl" => Self::Nl,
            "grep" => Self::Grep,
            "rg" => Self::Rg,
            "sed" => Self::Sed,
            "git" => Self::Git,
            "jq" => Self::Jq,
            "sha256sum" => Self::Sha256sum,
            "sha1sum" => Self::Sha1sum,
            "md5sum" => Self::Md5sum,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cd => "cd",
            Self::Ls => "ls",
            Self::Find => "find",
            Self::Mkdir => "mkdir",
            Self::Touch => "touch",
            Self::Rm => "rm",
            Self::Rmdir => "rmdir",
            Self::Mv => "mv",
            Self::Cp => "cp",
            Self::Cat => "cat",
            Self::Head => "head",
            Self::Tail => "tail",
            Self::Sort => "sort",
            Self::Uniq => "uniq",
            Self::Wc => "wc",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::Column => "column",
            Self::Tr => "tr",
            Self::File => "file",
            Self::Stat => "stat",
            Self::Diff => "diff",
            Self::Awk => "awk",
            Self::Strings => "strings",
            Self::Hexdump => "hexdump",
            Self::Od => "od",
            Self::Base64 => "base64",
            Self::Nl => "nl",
            Self::Grep => "grep",
            Self::Rg => "rg",
            Self::Sed => "sed",
            Self::Git => "git",
            Self::Jq => "jq",
            Self::Sha256sum => "sha256sum",
            Self::Sha1sum => "sha1sum",
            Self::Md5sum => "md5sum",
        }
    }
}

/// Maps to CC `COMMAND_OPERATION_TYPE`.
pub fn command_operation_type(command: PathCommand) -> FileOperationType {
    match command {
        PathCommand::Mkdir | PathCommand::Touch => FileOperationType::Create,
        PathCommand::Rm
        | PathCommand::Rmdir
        | PathCommand::Mv
        | PathCommand::Cp
        | PathCommand::Sed => FileOperationType::Write,
        _ => FileOperationType::Read,
    }
}

fn action_verb(command: PathCommand) -> &'static str {
    match command {
        PathCommand::Cd => "change directories to",
        PathCommand::Ls => "list files in",
        PathCommand::Find => "search files in",
        PathCommand::Mkdir => "create directories in",
        PathCommand::Touch => "create or modify files in",
        PathCommand::Rm => "remove files from",
        PathCommand::Rmdir => "remove directories from",
        PathCommand::Mv => "move files to/from",
        PathCommand::Cp => "copy files to/from",
        PathCommand::Cat => "concatenate files from",
        PathCommand::Head => "read the beginning of files from",
        PathCommand::Tail => "read the end of files from",
        PathCommand::Sort => "sort contents of files from",
        PathCommand::Uniq => "filter duplicate lines from files in",
        PathCommand::Wc => "count lines/words/bytes in files from",
        PathCommand::Cut => "extract columns from files in",
        PathCommand::Paste => "merge files from",
        PathCommand::Column => "format files from",
        PathCommand::Tr => "transform text from files in",
        PathCommand::File => "examine file types in",
        PathCommand::Stat => "read file stats from",
        PathCommand::Diff => "compare files from",
        PathCommand::Awk => "process text from files in",
        PathCommand::Strings => "extract strings from files in",
        PathCommand::Hexdump => "display hex dump of files from",
        PathCommand::Od => "display octal dump of files from",
        PathCommand::Base64 => "encode/decode files from",
        PathCommand::Nl => "number lines in files from",
        PathCommand::Grep | PathCommand::Rg => "search for patterns in files from",
        PathCommand::Sed => "edit files in",
        PathCommand::Git => "access files with git from",
        PathCommand::Jq => "process JSON from files in",
        PathCommand::Sha256sum => "compute SHA-256 checksums for files in",
        PathCommand::Sha1sum => "compute SHA-1 checksums for files in",
        PathCommand::Md5sum => "compute MD5 checksums for files in",
    }
}

/// Maps to CC local `filterOutFlags(args)`.
pub fn filter_out_flags(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut after_double_dash = false;
    for arg in args {
        if after_double_dash {
            result.push(arg.clone());
        } else if arg == "--" {
            after_double_dash = true;
        } else if !arg.starts_with('-') {
            result.push(arg.clone());
        }
    }
    result
}

/// Maps to CC local `parsePatternCommand(args, flagsWithArgs, defaults)`.
pub fn parse_pattern_command(
    args: &[String],
    flags_with_args: &[&str],
    defaults: &[&str],
) -> Vec<String> {
    let mut paths = Vec::new();
    let mut pattern_found = false;
    let mut after_double_dash = false;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            let flag = arg.split('=').next().unwrap_or(arg);
            if matches!(flag, "-e" | "--regexp" | "-f" | "--file") {
                pattern_found = true;
            }
            if flags_with_args.contains(&flag) && !arg.contains('=') {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if !pattern_found {
            pattern_found = true;
        } else {
            paths.push(arg.clone());
        }
        index += 1;
    }
    if paths.is_empty() {
        defaults.iter().map(|value| value.to_string()).collect()
    } else {
        paths
    }
}

/// Maps to CC `PATH_EXTRACTORS` entries.
pub fn extract_paths(command: PathCommand, args: &[String]) -> Vec<String> {
    match command {
        PathCommand::Cd => {
            if args.is_empty() {
                home_dir_string().map_or_else(|| vec!["~".to_string()], |home| vec![home])
            } else {
                vec![args.join(" ")]
            }
        }
        PathCommand::Ls => non_empty_or_default(filter_out_flags(args), "."),
        PathCommand::Find => extract_find_paths(args),
        PathCommand::Mkdir
        | PathCommand::Touch
        | PathCommand::Rm
        | PathCommand::Rmdir
        | PathCommand::Mv
        | PathCommand::Cp
        | PathCommand::Cat
        | PathCommand::Head
        | PathCommand::Tail
        | PathCommand::Sort
        | PathCommand::Uniq
        | PathCommand::Wc
        | PathCommand::Cut
        | PathCommand::Paste
        | PathCommand::Column
        | PathCommand::File
        | PathCommand::Stat
        | PathCommand::Diff
        | PathCommand::Awk
        | PathCommand::Strings
        | PathCommand::Hexdump
        | PathCommand::Od
        | PathCommand::Base64
        | PathCommand::Nl
        | PathCommand::Sha256sum
        | PathCommand::Sha1sum
        | PathCommand::Md5sum => filter_out_flags(args),
        PathCommand::Tr => {
            let has_delete = args.iter().any(|arg| {
                arg == "-d" || arg == "--delete" || (arg.starts_with('-') && arg.contains('d'))
            });
            let non_flags = filter_out_flags(args);
            non_flags
                .into_iter()
                .skip(if has_delete { 1 } else { 2 })
                .collect()
        }
        PathCommand::Grep => {
            let flags = [
                "-e",
                "--regexp",
                "-f",
                "--file",
                "--exclude",
                "--include",
                "--exclude-dir",
                "--include-dir",
                "-m",
                "--max-count",
                "-A",
                "--after-context",
                "-B",
                "--before-context",
                "-C",
                "--context",
            ];
            let paths = parse_pattern_command(args, &flags, &[]);
            if paths.is_empty()
                && args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-r" | "-R" | "--recursive"))
            {
                vec![".".to_string()]
            } else {
                paths
            }
        }
        PathCommand::Rg => {
            let flags = [
                "-e",
                "--regexp",
                "-f",
                "--file",
                "-t",
                "--type",
                "-T",
                "--type-not",
                "-g",
                "--glob",
                "-m",
                "--max-count",
                "--max-depth",
                "-r",
                "--replace",
                "-A",
                "--after-context",
                "-B",
                "--before-context",
                "-C",
                "--context",
            ];
            parse_pattern_command(args, &flags, &["."])
        }
        PathCommand::Sed => extract_sed_paths(args),
        PathCommand::Git => extract_git_paths(args),
        PathCommand::Jq => extract_jq_paths(args),
    }
}

fn non_empty_or_default(values: Vec<String>, default_value: &str) -> Vec<String> {
    if values.is_empty() {
        vec![default_value.to_string()]
    } else {
        values
    }
}

fn extract_find_paths(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let path_flags = [
        "-newer",
        "-anewer",
        "-cnewer",
        "-mnewer",
        "-samefile",
        "-path",
        "-wholename",
        "-ilname",
        "-lname",
        "-ipath",
        "-iwholename",
    ];
    let mut found_non_global_flag = false;
    let mut after_double_dash = false;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if after_double_dash {
            paths.push(arg.clone());
            index += 1;
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            if matches!(arg.as_str(), "-H" | "-L" | "-P") {
                index += 1;
                continue;
            }
            found_non_global_flag = true;
            if path_flags.contains(&arg.as_str()) || is_newer_variant(arg) {
                if let Some(next) = args.get(index + 1) {
                    paths.push(next.clone());
                    index += 2;
                    continue;
                }
            }
            index += 1;
            continue;
        }
        if !found_non_global_flag {
            paths.push(arg.clone());
        }
        index += 1;
    }
    non_empty_or_default(paths, ".")
}

fn is_newer_variant(arg: &str) -> bool {
    let bytes = arg.as_bytes();
    bytes.len() == 8
        && arg.starts_with("-newer")
        && matches!(bytes[6] as char, 'a' | 'c' | 'm' | 'B' | 't')
        && matches!(bytes[7] as char, 'a' | 'c' | 'm' | 'B' | 't')
}

fn extract_sed_paths(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut skip_next = false;
    let mut script_found = false;
    let mut after_double_dash = false;
    let mut index = 0usize;
    while index < args.len() {
        if skip_next {
            skip_next = false;
            index += 1;
            continue;
        }
        let arg = &args[index];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            if matches!(arg.as_str(), "-f" | "--file") {
                if let Some(script_file) = args.get(index + 1) {
                    paths.push(script_file.clone());
                    skip_next = true;
                }
                script_found = true;
            } else if matches!(arg.as_str(), "-e" | "--expression") {
                skip_next = true;
                script_found = true;
            } else if arg.contains('e') || arg.contains('f') {
                script_found = true;
            }
            index += 1;
            continue;
        }
        if !script_found {
            script_found = true;
        } else {
            paths.push(arg.clone());
        }
        index += 1;
    }
    paths
}

fn extract_git_paths(args: &[String]) -> Vec<String> {
    if args.first().is_some_and(|arg| arg == "diff") && args.iter().any(|arg| arg == "--no-index") {
        return filter_out_flags(&args[1..]).into_iter().take(2).collect();
    }
    Vec::new()
}

fn extract_jq_paths(args: &[String]) -> Vec<String> {
    let flags_with_args = [
        "-e",
        "--expression",
        "-f",
        "--from-file",
        "--arg",
        "--argjson",
        "--slurpfile",
        "--rawfile",
        "--args",
        "--jsonargs",
        "-L",
        "--library-path",
        "--indent",
        "--tab",
    ];
    let mut paths = Vec::new();
    let mut filter_found = false;
    let mut after_double_dash = false;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            let flag = arg.split('=').next().unwrap_or(arg);
            if matches!(flag, "-e" | "--expression") {
                filter_found = true;
            }
            if flags_with_args.contains(&flag) && !arg.contains('=') {
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if !filter_found {
            filter_found = true;
        } else {
            paths.push(arg.clone());
        }
        index += 1;
    }
    paths
}

/// Maps to CC local `checkDangerousRemovalPaths(command, args, cwd)`.
pub fn check_dangerous_removal_paths(
    command: PathCommand,
    args: &[String],
    cwd: &str,
) -> PermissionResult {
    if !matches!(command, PathCommand::Rm | PathCommand::Rmdir) {
        return passthrough(format!(
            "No dangerous removals detected for {} command",
            command.as_str()
        ));
    }
    for path in extract_paths(command, args) {
        let clean_path = expand_tilde(path.trim_matches(['\'', '"']));
        let absolute_path = if Path::new(&clean_path).is_absolute() {
            clean_path
        } else {
            PathBuf::from(cwd).join(clean_path).display().to_string()
        };
        if is_dangerous_removal_path(&absolute_path) {
            let message = format!(
                "Dangerous {} operation detected: '{}'\n\nThis command would remove a critical system directory. This requires explicit approval and cannot be auto-allowed by permission rules.",
                command.as_str(),
                absolute_path
            );
            return PermissionResult::Ask {
                message,
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: format!(
                        "Dangerous {} operation on critical path: {}",
                        command.as_str(),
                        absolute_path
                    ),
                }),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }
    passthrough(format!(
        "No dangerous removals detected for {} command",
        command.as_str()
    ))
}

/// Maps to CC local `validateCommandPaths(...)`.
pub fn validate_command_paths(
    command: PathCommand,
    args: &[String],
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
    operation_type_override: Option<FileOperationType>,
) -> PermissionResult {
    let operation_type = operation_type_override.unwrap_or_else(|| command_operation_type(command));
    if matches!(command, PathCommand::Mv | PathCommand::Cp)
        && args.iter().any(|arg| arg.starts_with('-'))
    {
        return PermissionResult::Ask {
            message: format!(
                "{} with flags requires manual approval to ensure path safety. For security, Claude Code cannot automatically validate {} commands that use flags, as some flags like --target-directory=PATH can bypass path validation.",
                command.as_str(),
                command.as_str()
            ),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: format!(
                    "{} command with flags requires manual approval",
                    command.as_str()
                ),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    if compound_command_has_cd && operation_type != FileOperationType::Read {
        return PermissionResult::Ask {
            message: "Commands that change directories and perform write operations require explicit approval to ensure paths are evaluated correctly. For security, Claude Code cannot automatically determine the final working directory when 'cd' is used in compound commands."
                .to_string(),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Compound command contains cd with write operation - manual approval required to prevent path resolution bypass"
                    .to_string(),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    for path in extract_paths(command, args) {
        let result = validate_path(&path, cwd, tool_permission_context, operation_type);
        if !result.allowed {
            let working_dirs = all_working_directories(tool_permission_context);
            let dir_list = format_directory_list(&working_dirs);
            let message = match &result.decision_reason {
                Some(PermissionDecisionReason::Other { reason })
                | Some(PermissionDecisionReason::SafetyCheck { reason, .. }) => reason.clone(),
                _ => format!(
                    "{} in '{}' was blocked. For security, Claude Code may only {} the allowed working directories for this session: {}.",
                    command.as_str(),
                    result.resolved_path,
                    action_verb(command),
                    dir_list
                ),
            };
            if matches!(
                result.decision_reason,
                Some(PermissionDecisionReason::Rule { .. })
            ) {
                return PermissionResult::Deny {
                    message,
                    decision_reason: result.decision_reason.unwrap(),
                    tool_use_id: None,
                };
            }
            return PermissionResult::Ask {
                message,
                updated_input: None,
                decision_reason: result.decision_reason,
                suggestions: Vec::new(),
                blocked_path: Some(result.resolved_path),
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    passthrough(format!(
        "Path validation passed for {} command",
        command.as_str()
    ))
}

/// Maps to CC `createPathChecker(command, operationTypeOverride)`.
pub fn check_path_command(
    command: PathCommand,
    args: &[String],
    cwd: &str,
    context: &ToolPermissionContext,
    compound_command_has_cd: bool,
    operation_type_override: Option<FileOperationType>,
) -> PermissionResult {
    let result = validate_command_paths(
        command,
        args,
        cwd,
        context,
        compound_command_has_cd,
        operation_type_override,
    );
    if matches!(result, PermissionResult::Deny { .. }) {
        return result;
    }
    if matches!(command, PathCommand::Rm | PathCommand::Rmdir) {
        let dangerous = check_dangerous_removal_paths(command, args, cwd);
        if !matches!(dangerous, PermissionResult::Passthrough { .. }) {
            return dangerous;
        }
    }
    match result {
        PermissionResult::Ask {
            message,
            decision_reason,
            blocked_path,
            metadata,
            pending_classifier_check,
            ..
        } => {
            let operation_type =
                operation_type_override.unwrap_or_else(|| command_operation_type(command));
            let mut suggestions = Vec::new();
            if let Some(blocked_path) = blocked_path.as_ref() {
                if operation_type == FileOperationType::Read {
                    if let Some(update) =
                        crate::utils::permissions::permission_update::create_read_rule_suggestion(
                            &get_directory_for_path(blocked_path),
                            PermissionUpdateDestination::Session,
                        )
                    {
                        suggestions.push(update);
                    }
                } else {
                    suggestions.push(PermissionUpdate::AddDirectories {
                        destination: PermissionUpdateDestination::Session,
                        directories: vec![get_directory_for_path(blocked_path)],
                    });
                }
            }
            if matches!(
                operation_type,
                FileOperationType::Write | FileOperationType::Create
            ) {
                suggestions.push(PermissionUpdate::SetMode {
                    destination: PermissionUpdateDestination::Session,
                    mode: PermissionMode::AcceptEdits,
                });
            }
            PermissionResult::Ask {
                message,
                updated_input: None,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check,
                content_blocks: Vec::new(),
            }
        }
        other => other,
    }
}

/// Maps to CC local `parseCommandArguments(cmd)`.
pub fn parse_command_arguments(command: &str) -> Vec<String> {
    let Ok(arguments) = crate::utils::bash::shell_quote::try_parse_shell_command(command) else {
        return Vec::new();
    };
    arguments
        .into_iter()
        .filter_map(|argument| match argument {
            crate::utils::bash::shell_quote::ParseEntry::String(value)
            | crate::utils::bash::shell_quote::ParseEntry::Glob(value) => Some(value),
            crate::utils::bash::shell_quote::ParseEntry::Operator(_)
            | crate::utils::bash::shell_quote::ParseEntry::Comment(_) => None,
        })
        .collect()
}

/// Maps to CC local `validateSinglePathCommand(...)`.
pub fn validate_single_path_command(
    cmd: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
) -> PermissionResult {
    let stripped_cmd = super::bash_permissions::strip_safe_wrappers(cmd);
    let extracted_args = parse_command_arguments(&stripped_cmd);
    if extracted_args.is_empty() {
        return passthrough("Empty command - no paths to validate".to_string());
    }
    let Some((base_cmd, args)) = extracted_args.split_first() else {
        return passthrough("Empty command - no paths to validate".to_string());
    };
    let Some(path_command) = PathCommand::parse(base_cmd) else {
        return passthrough(format!(
            "Command '{base_cmd}' is not a path-restricted command"
        ));
    };
    let stripped = stripped_cmd.as_str();
    let operation_type_override = if path_command == PathCommand::Sed
        && super::sed_validation::sed_command_is_allowed_by_allowlist(
            stripped,
            super::sed_validation::SedValidationOptions::default(),
        ) {
        Some(FileOperationType::Read)
    } else {
        None
    };
    check_path_command(
        path_command,
        args,
        cwd,
        tool_permission_context,
        compound_command_has_cd,
        operation_type_override,
    )
}

/// Maps to CC local `validateOutputRedirections(...)`.
pub fn validate_output_redirections(
    redirections: &[OutputRedirection],
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
) -> PermissionResult {
    if compound_command_has_cd && !redirections.is_empty() {
        return PermissionResult::Ask {
            message: "Commands that change directories and write via output redirection require explicit approval to ensure paths are evaluated correctly. For security, Claude Code cannot automatically determine the final working directory when 'cd' is used in compound commands."
                .to_string(),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Compound command contains cd with output redirection - manual approval required to prevent path resolution bypass"
                    .to_string(),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }
    for redirection in redirections {
        if redirection.target == "/dev/null" {
            continue;
        }
        let result = validate_path(
            &redirection.target,
            cwd,
            tool_permission_context,
            FileOperationType::Create,
        );
        if !result.allowed {
            let working_dirs = all_working_directories(tool_permission_context);
            let dir_list = format_directory_list(&working_dirs);
            let message = match &result.decision_reason {
                Some(PermissionDecisionReason::Other { reason })
                | Some(PermissionDecisionReason::SafetyCheck { reason, .. }) => reason.clone(),
                Some(PermissionDecisionReason::Rule { .. }) => {
                    format!(
                        "Output redirection to '{}' was blocked by a deny rule.",
                        result.resolved_path
                    )
                }
                _ => format!(
                    "Output redirection to '{}' was blocked. For security, Claude Code may only write to files in the allowed working directories for this session: {}.",
                    result.resolved_path, dir_list
                ),
            };
            if matches!(
                result.decision_reason,
                Some(PermissionDecisionReason::Rule { .. })
            ) {
                return PermissionResult::Deny {
                    message,
                    decision_reason: result.decision_reason.unwrap(),
                    tool_use_id: None,
                };
            }
            return PermissionResult::Ask {
                message,
                updated_input: None,
                decision_reason: result.decision_reason,
                suggestions: vec![PermissionUpdate::AddDirectories {
                    destination: PermissionUpdateDestination::Session,
                    directories: vec![get_directory_for_path(&result.resolved_path)],
                }],
                blocked_path: Some(result.resolved_path),
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }
    passthrough("No unsafe redirections found".to_string())
}

/// Maps to CC `checkPathConstraints(input, cwd, toolPermissionContext, ...)`.
pub fn check_path_constraints(
    command: &str,
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
) -> PermissionResult {
    if contains_process_substitution(command) {
        return PermissionResult::Ask {
            message: "Process substitution (>(...) or <(...)) can execute arbitrary commands and requires manual approval".to_string(),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Process substitution requires manual approval".to_string(),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let extracted = crate::utils::bash::commands::extract_output_redirections(command);
    if extracted.has_dangerous_redirection {
        return PermissionResult::Ask {
            message: "Shell expansion syntax in paths requires manual approval".to_string(),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Shell expansion syntax in paths requires manual approval".to_string(),
            }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }
    let redirection_result = validate_output_redirections(
        &extracted.redirections,
        cwd,
        tool_permission_context,
        compound_command_has_cd,
    );
    if !matches!(redirection_result, PermissionResult::Passthrough { .. }) {
        return redirection_result;
    }

    for cmd in crate::utils::bash::commands::split_command_deprecated(command) {
        let result = validate_single_path_command(
            &cmd,
            cwd,
            tool_permission_context,
            compound_command_has_cd,
        );
        if matches!(
            result,
            PermissionResult::Ask { .. } | PermissionResult::Deny { .. }
        ) {
            return result;
        }
    }

    passthrough("All path commands validated successfully".to_string())
}

/// AST-backed half of CC `checkPathConstraints(...)`.
pub fn check_path_constraints_from_ast(
    commands: &[crate::utils::bash::ast::SimpleCommand],
    cwd: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    let compound_command_has_cd = commands
        .iter()
        .filter(|command| {
            strip_wrappers_from_argv(&command.argv)
                .first()
                .is_some_and(|name| matches!(name.as_str(), "cd" | "pushd" | "popd"))
        })
        .count()
        > 0;

    let redirections = commands
        .iter()
        .flat_map(|command| command.redirects.iter())
        .filter_map(|redirect| match redirect.op.as_str() {
            ">" | ">|" | "&>" => Some(OutputRedirection {
                target: redirect.target.clone(),
                operator: ">".to_string(),
            }),
            ">>" | "&>>" => Some(OutputRedirection {
                target: redirect.target.clone(),
                operator: ">>".to_string(),
            }),
            ">&" if !redirect.target.chars().all(|ch| ch.is_ascii_digit()) => {
                Some(OutputRedirection {
                    target: redirect.target.clone(),
                    operator: ">".to_string(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let redirection_result = validate_output_redirections(
        &redirections,
        cwd,
        tool_permission_context,
        compound_command_has_cd,
    );
    if !matches!(redirection_result, PermissionResult::Passthrough { .. }) {
        return redirection_result;
    }

    for command in commands {
        let argv = strip_wrappers_from_argv(&command.argv);
        let Some((name, args)) = argv.split_first() else {
            continue;
        };
        let Some(path_command) = PathCommand::parse(name) else {
            continue;
        };
        let operation_type_override = if path_command == PathCommand::Sed
            && super::sed_validation::sed_command_is_allowed_by_allowlist(
                &super::bash_permissions::strip_safe_wrappers(&command.text),
                super::sed_validation::SedValidationOptions::default(),
            ) {
            Some(FileOperationType::Read)
        } else {
            None
        };
        let result = check_path_command(
            path_command,
            args,
            cwd,
            tool_permission_context,
            compound_command_has_cd,
            operation_type_override,
        );
        if matches!(
            result,
            PermissionResult::Ask { .. } | PermissionResult::Deny { .. }
        ) {
            return result;
        }
    }
    passthrough("All AST path commands validated successfully".to_string())
}

/// Maps to CC canonical `stripWrappersFromArgv(argv)` in pathValidation.ts.
pub fn strip_wrappers_from_argv(argv: &[String]) -> Vec<String> {
    let mut current = argv.to_vec();
    loop {
        let Some(name) = current.first().map(String::as_str) else {
            return current;
        };
        match name {
            "time" | "nohup" => {
                current.drain(
                    ..if current.get(1).is_some_and(|arg| arg == "--") {
                        2
                    } else {
                        1
                    },
                );
            }
            "timeout" => {
                let Some(duration_index) = timeout_duration_index(&current) else {
                    return current;
                };
                if !is_timeout_duration(&current[duration_index]) {
                    return current;
                }
                current.drain(..=duration_index);
            }
            "nice" => {
                let consumed = if current.get(1).is_some_and(|arg| arg == "-n")
                    && current.get(2).is_some_and(|arg| is_integer(arg))
                {
                    if current.get(3).is_some_and(|arg| arg == "--") {
                        4
                    } else {
                        3
                    }
                } else if current
                    .get(1)
                    .is_some_and(|arg| arg.starts_with('-') && arg.len() > 1 && is_integer(arg))
                {
                    if current.get(2).is_some_and(|arg| arg == "--") {
                        3
                    } else {
                        2
                    }
                } else if current.get(1).is_some_and(|arg| arg == "--") {
                    2
                } else {
                    1
                };
                current.drain(..consumed.min(current.len()));
            }
            "stdbuf" => {
                let Some(index) = stdbuf_command_index(&current) else {
                    return current;
                };
                current.drain(..index);
            }
            "env" => {
                let Some(index) = env_command_index(&current) else {
                    return current;
                };
                current.drain(..index);
            }
            _ => return current,
        }
    }
}

fn safe_wrapper_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '+' | '-'))
}

fn timeout_duration_index(argv: &[String]) -> Option<usize> {
    let mut index = 1usize;
    while let Some(arg) = argv.get(index).map(String::as_str) {
        if matches!(
            arg,
            "--foreground" | "--preserve-status" | "--verbose" | "-v"
        ) || ((arg.starts_with("--kill-after=") || arg.starts_with("--signal="))
            && arg
                .split_once('=')
                .is_some_and(|(_, value)| safe_wrapper_value(value)))
            || ((arg.starts_with("-k") || arg.starts_with("-s"))
                && arg.len() > 2
                && safe_wrapper_value(&arg[2..]))
        {
            index += 1;
        } else if matches!(arg, "--kill-after" | "--signal" | "-k" | "-s") {
            if !argv
                .get(index + 1)
                .is_some_and(|value| safe_wrapper_value(value))
            {
                return None;
            }
            index += 2;
        } else if arg == "--" {
            return Some(index + 1);
        } else if arg.starts_with('-') {
            return None;
        } else {
            return Some(index);
        }
    }
    None
}

fn is_timeout_duration(value: &str) -> bool {
    let numeric = value
        .strip_suffix('s')
        .or_else(|| value.strip_suffix('m'))
        .or_else(|| value.strip_suffix('h'))
        .or_else(|| value.strip_suffix('d'))
        .unwrap_or(value);
    let mut dot = false;
    !numeric.is_empty()
        && !numeric.starts_with('.')
        && !numeric.ends_with('.')
        && numeric.chars().all(|ch| {
            if ch == '.' && !dot {
                dot = true;
                true
            } else {
                ch.is_ascii_digit()
            }
        })
}

fn is_integer(value: &str) -> bool {
    let numeric = value.strip_prefix('-').unwrap_or(value);
    !numeric.is_empty() && numeric.chars().all(|ch| ch.is_ascii_digit())
}

fn stdbuf_command_index(argv: &[String]) -> Option<usize> {
    let mut index = 1usize;
    while let Some(arg) = argv.get(index).map(String::as_str) {
        if matches!(arg, "-i" | "-o" | "-e") && argv.get(index + 1).is_some() {
            index += 2;
        } else if ((arg.starts_with("-i") || arg.starts_with("-o") || arg.starts_with("-e"))
            && arg.len() > 2)
            || arg.starts_with("--input=")
            || arg.starts_with("--output=")
            || arg.starts_with("--error=")
        {
            index += 1;
        } else if arg.starts_with('-') {
            return None;
        } else {
            return (index > 1).then_some(index);
        }
    }
    None
}

fn env_command_index(argv: &[String]) -> Option<usize> {
    let mut index = 1usize;
    while let Some(arg) = argv.get(index).map(String::as_str) {
        if (arg.contains('=') && !arg.starts_with('-')) || matches!(arg, "-i" | "-0" | "-v") {
            index += 1;
        } else if arg == "-u" && argv.get(index + 1).is_some() {
            index += 2;
        } else if arg.starts_with('-') {
            return None;
        } else {
            return Some(index);
        }
    }
    None
}

fn contains_process_substitution(command: &str) -> bool {
    command.contains(">(")
        || command.contains("<(")
        || command.contains("> >(")
        || command.contains(">>(")
}

fn passthrough(message: String) -> PermissionResult {
    PermissionResult::Passthrough {
        message,
        decision_reason: None,
        suggestions: Vec::new(),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

fn home_dir_string() -> Option<String> {
    crate::utils::process_env::var_os("HOME").map(|home| PathBuf::from(home).display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_with_allowed_dir(dir: &Path) -> ToolPermissionContext {
        let mut context = ToolPermissionContext::default();
        context.additional_working_directories.insert(
            dir.display().to_string(),
            AdditionalWorkingDirectory {
                path: dir.display().to_string(),
                source: PermissionRuleSource::Session,
            },
        );
        context
    }

    #[test]
    fn path_extractors_match_official_double_dash_and_defaults() {
        let rm_args = vec![
            "--".to_string(),
            "-/../.claude/settings.local.json".to_string(),
        ];
        assert_eq!(
            filter_out_flags(&rm_args),
            vec!["-/../.claude/settings.local.json".to_string()]
        );
        assert_eq!(extract_paths(PathCommand::Ls, &[]), vec![".".to_string()]);
        assert_eq!(extract_paths(PathCommand::Find, &[]), vec![".".to_string()]);
        assert_eq!(
            extract_paths(PathCommand::Grep, &["-r".to_string(), "needle".to_string()]),
            vec![".".to_string()]
        );
        assert_eq!(
            extract_paths(PathCommand::Rg, &["needle".to_string()]),
            vec![".".to_string()]
        );
        assert_eq!(
            extract_paths(
                PathCommand::Git,
                &[
                    "diff".to_string(),
                    "--no-index".to_string(),
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                ],
            ),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn command_argument_parser_delegates_to_shell_quote_owner() {
        assert_eq!(
            parse_command_arguments("grep '' *.rs /tmp/file"),
            ["grep", "", "*.rs", "/tmp/file"]
        );
        assert_eq!(
            parse_command_arguments("echo x > out"),
            ["echo", "x", "out"]
        );
        assert!(parse_command_arguments("echo ${}").is_empty());
    }

    #[test]
    fn dangerous_removal_paths_override_allowlike_results() {
        let result = check_dangerous_removal_paths(PathCommand::Rm, &["/".to_string()], "/tmp");
        assert!(matches!(
            result,
            PermissionResult::Ask { ref message, decision_reason: Some(PermissionDecisionReason::Other { .. }), .. }
                if message.contains("Dangerous rm operation detected: '/'")
        ));
    }

    #[test]
    fn path_constraints_pass_allowed_dir_and_block_outside_redirection() {
        let allowed =
            std::env::temp_dir().join(format!("cometix-path-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&allowed).unwrap();
        let context = context_with_allowed_dir(&allowed);
        let cwd = allowed.display().to_string();

        assert!(matches!(
            check_path_constraints("ls .", &cwd, &context, false),
            PermissionResult::Passthrough { ref message, .. }
                if message == "All path commands validated successfully"
        ));

        let outside =
            std::env::temp_dir().join(format!("cometix-outside-{}", uuid::Uuid::new_v4().simple()));
        let outside_file = outside.join("out.txt");
        let result = check_path_constraints(
            &format!("echo hi > {}", outside_file.display()),
            &cwd,
            &context,
            false,
        );
        let _ = std::fs::remove_dir_all(&allowed);
        assert!(matches!(
            result,
            PermissionResult::Ask { ref message, ref suggestions, .. }
                if message.contains("Output redirection") && suggestions.len() == 1
        ));
    }

    #[test]
    fn compound_cd_write_and_mv_cp_flags_require_manual_approval() {
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let context = ToolPermissionContext::default();
        assert!(matches!(
            validate_command_paths(
                PathCommand::Rm,
                &["file.txt".to_string()],
                &cwd,
                &context,
                true,
                None,
            ),
            PermissionResult::Ask { ref message, .. }
                if message.starts_with("Commands that change directories and perform write operations")
        ));
        assert!(matches!(
            validate_command_paths(
                PathCommand::Mv,
                &["--target-directory=/tmp".to_string(), "a".to_string()],
                &cwd,
                &context,
                false,
                None,
            ),
            PermissionResult::Ask { ref message, .. }
                if message.starts_with("mv with flags requires manual approval")
        ));
    }

    #[test]
    fn ast_path_constraints_validate_redirects_and_wrapped_commands() {
        let allowed = std::env::temp_dir().join(format!(
            "cometix-ast-path-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-ast-path-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&allowed).unwrap();
        let context = context_with_allowed_dir(&allowed);
        let command = format!("echo hi 2>&1 > {}", outside.join("out").display());
        let crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } =
            crate::utils::bash::ast::parse_for_security(&command)
        else {
            panic!("static redirect command parses");
        };
        assert!(matches!(
            check_path_constraints_from_ast(
                &commands,
                &allowed.display().to_string(),
                &context,
            ),
            PermissionResult::Ask { ref message, .. } if message.contains("Output redirection")
        ));

        let wrapped = format!("env FOO=x stdbuf -o0 timeout 5 rm {}", outside.display());
        let crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } =
            crate::utils::bash::ast::parse_for_security(&wrapped)
        else {
            panic!("wrapped command parses");
        };
        assert!(matches!(
            check_path_constraints_from_ast(&commands, &allowed.display().to_string(), &context,),
            PermissionResult::Ask { .. }
        ));
        let _ = std::fs::remove_dir_all(allowed);
    }

    #[test]
    fn strip_wrappers_from_argv_matches_canonical_ast_path_owner() {
        assert_eq!(
            strip_wrappers_from_argv(&[
                "timeout".to_string(),
                "10".to_string(),
                "rm".to_string(),
                "file".to_string(),
            ]),
            vec!["rm".to_string(), "file".to_string()]
        );
        assert_eq!(
            strip_wrappers_from_argv(&[
                "env".to_string(),
                "FOO=x".to_string(),
                "stdbuf".to_string(),
                "-o0".to_string(),
                "nice".to_string(),
                "-n".to_string(),
                "5".to_string(),
                "cat".to_string(),
                "file".to_string(),
            ]),
            vec!["cat".to_string(), "file".to_string()]
        );
    }
}
