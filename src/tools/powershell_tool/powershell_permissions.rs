//! Maps to: CC `tools/PowerShellTool/powershellPermissions.ts`.
//!
//! The native parser slice is conservative rather than fail-open: explicit
//! deny rules are checked against every compound fragment (including aliases,
//! assignments, invocation operators, and parse-degraded input) before ask or
//! allow decisions. Security/provider/removal checks remain bypass-immune.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionBehavior, PermissionRule};
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use crate::utils::permissions::shell_rule_matching::{
    ShellPermissionRule, match_wildcard_pattern, parse_permission_rule,
    suggestion_for_exact_command,
};

use crate::utils::powershell::parser::{
    CommandNameType, ParsedPowerShellCommand, classify_command_name, get_all_commands,
    get_file_redirections, parse_powershell_command,
};

use super::git_safety::{PS_TOKENIZER_DASH_CHARS, is_dot_git_path_ps, is_git_internal_path_ps};
use super::mode_validation::check_permission_mode;
use super::path_validation::{
    check_path_constraints, dangerous_removal_deny, is_dangerous_removal_raw_path,
};
use super::powershell_security::powershell_command_is_safe;
use super::read_only_validation::{
    arg_leaks_value, is_cwd_changing_cmdlet, is_read_only_command, resolve_to_canonical,
};
use super::tool_name::POWERSHELL_TOOL_NAME;

/// Maps to: CC `powershellPermissions.ts:70-84#GIT_SAFETY_WRITE_CMDLETS`.
const GIT_SAFETY_WRITE_CMDLETS: [&str; 13] = [
    "new-item",
    "set-content",
    "add-content",
    "out-file",
    "copy-item",
    "move-item",
    "rename-item",
    "expand-archive",
    "invoke-webrequest",
    "invoke-restmethod",
    "tee-object",
    "export-csv",
    "export-clixml",
];

/// Maps to: CC `powershellToolHasPermission(...)`.
pub fn powershell_tool_has_permission(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    let command = command.trim();
    if command.is_empty() {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Empty command is safe".to_string(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    // Parse the command once and thread it through every AST-backed check.
    let parsed = parse_powershell_command(command);

    // SECURITY: deny/ask rules are checked BEFORE the parse-validity gate.
    // They operate on the raw command string, so explicit deny rules keep
    // blocking commands even when parsing fails.
    //
    // Only `deny` early-returns. Maps to: CC `powershellPermissions.ts:671-673`
    // (exact match: deny returns, ask falls through) and `:694-711` (prefix
    // match: the ask is stored in `preParseAskDecision` and pushed into
    // `decisions[]` instead of returning). CC's comment at :694-700 names the
    // bug an early return reintroduces: `Get-Process; Invoke-Expression evil`
    // under ask(Get-Process:*) showed the ask dialog and the later deny — from
    // the sub-command scan, the dangerous-removal hard-deny, or path
    // constraints — never fired.
    let mut exact_allow = None;
    let mut pre_parse_ask = None;
    if let Some(result) = match_exact_or_content_rules(command, tool_permission_context) {
        match result {
            PermissionResult::Deny { .. } => return result,
            PermissionResult::Ask { .. } => pre_parse_ask = Some(result),
            PermissionResult::Allow { .. } => exact_allow = Some(result),
            PermissionResult::Passthrough { .. } => {}
        }
    }

    // Maps to: CC `powershellPermissions.ts:747-758`. A rule-driven allow may
    // short-circuit only while the AST is unavailable, and never for an
    // application name: input-side `strip_module_prefix` collapses
    // `scripts\build.exe --flag` to `build --flag`, so an exact `build:*` allow
    // would otherwise approve running the local script. `Get-Date` and other
    // module-qualified names classify as applications too, which downgrades
    // their allow to an ask while pwsh is degraded — the fail-safe direction.
    //
    // CC's guard is `preParseAskDecision === null` (:753), which covers BOTH a
    // deferred prefix ask and the raw-string UNC ask — the port previously
    // tested only the UNC half, so an ask rule could be silently overridden by
    // an exact allow on the parse-failed path (CC :725-730 calls this out).
    if let Some(result) = exact_allow.as_ref() {
        if !parsed.valid
            && pre_parse_ask.is_none()
            && !contains_vulnerable_unc_path(command)
            && classify_command_name(command.split_whitespace().next().unwrap_or_default())
                != CommandNameType::Application
        {
            return result.clone();
        }
    }

    if let Some(path) = dangerous_removal_path(command) {
        return dangerous_removal_deny(&path);
    }

    // COLLECT-THEN-REDUCE: post-parse decisions resolve as
    // deny > ask > allow > passthrough, with the first of each behavior
    // winning. This structurally prevents an earlier `ask` (security flags,
    // provider paths, git safety) from masking a later `deny` from path
    // constraints.
    let mut decisions: Vec<PermissionResult> = Vec::new();

    // Maps to: CC `powershellPermissions.ts:902-907` — the deferred pre-parse
    // ask is pushed FIRST so its rule-attributed message wins among asks
    // (first-of-behavior wins), while any deny collected below still beats it.
    if let Some(result) = pre_parse_ask {
        decisions.push(result);
    }

    if contains_vulnerable_unc_path(command) {
        decisions.push(safety_ask(
            command,
            "Command contains a UNC path that could trigger network requests",
        ));
    }
    if contains_non_filesystem_provider(command) {
        decisions.push(safety_ask(
            command,
            "Command uses a non-filesystem PowerShell provider and requires approval",
        ));
    }

    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_COMMAND_INJECTION_CHECK")
            .as_deref(),
    ) {
        if let PermissionResult::Ask {
            decision_reason: Some(PermissionDecisionReason::SafetyCheck { reason, .. }),
            ..
        } = powershell_command_is_safe(command)
        {
            let decision_reason = PermissionDecisionReason::SafetyCheck {
                reason,
                classifier_approvable: true,
            };
            decisions.push(PermissionResult::Ask {
                message: create_permission_request_message(Some(&decision_reason)),
                updated_input: None,
                decision_reason: Some(decision_reason),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            });
        }
    }

    // CC pushes the git-safety asks after the security-check ask
    // (`powershellPermissions.ts:912`, `:1168-1257`), and first-of-behavior
    // wins in its reduce, so they stay behind it here too.
    if let Some(reason) = git_safety_write_ask_reason(command) {
        decisions.push(safety_ask(command, reason));
    }

    // Decision: path constraints (`powershellPermissions.ts:1271-1276`). The
    // deny-capable check. `has_cd_sub_command` is threaded through so a
    // compound containing a cwd-changing cmdlet forces an ask for every
    // statement with path operations — relative paths would otherwise resolve
    // against the stale validator cwd instead of PowerShell's runtime cwd.
    let has_cd_sub_command = compound_command_has_cd(&parsed);
    let path_result = check_path_constraints(&parsed, tool_permission_context, has_cd_sub_command);
    if !matches!(path_result, PermissionResult::Passthrough { .. }) {
        decisions.push(path_result);
    }

    // Decision: rule-driven allow (`powershellPermissions.ts:1306-1316`). It
    // joins the reduce instead of returning early so a path-constraint deny
    // still wins. Every sub-command must carry a non-application name that
    // leaks no argument value: the canonical form collapses newlines, so a
    // second statement could otherwise ride in on the first one's exact rule.
    if let Some(result) = exact_allow {
        let commands = get_all_commands(&parsed);
        if !commands.is_empty()
            && commands.iter().all(|element| {
                element.name_type != Some(CommandNameType::Application)
                    && !arg_leaks_value(Some(element))
            })
        {
            decisions.push(result);
        }
    }

    // Decision: read-only allowlist (`powershellPermissions.ts:1322-1331`).
    if is_read_only_command(command, Some(&parsed)) {
        decisions.push(PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Command is read-only and safe to execute".to_string(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        });
    }

    // Decision: file redirections (`powershellPermissions.ts:1337-1345`).
    // `is_read_only_command` already rejects redirections internally, so this
    // cannot conflict with the read-only allow above.
    if !get_file_redirections(&parsed).is_empty() {
        decisions.push(PermissionResult::Ask {
            message: "Command contains file redirections that could write to arbitrary paths"
                .to_string(),
            updated_input: None,
            decision_reason: None,
            suggestions: suggestion_for_exact_command(POWERSHELL_TOOL_NAME, command),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        });
    }

    // Decision: mode-specific handling (`powershellPermissions.ts:1349-1352`).
    // `check_permission_mode` only ever returns allow or passthrough.
    let mode_result = check_permission_mode(command, &parsed, tool_permission_context);
    if !matches!(mode_result, PermissionResult::Passthrough { .. }) {
        decisions.push(mode_result);
    }

    // REDUCE: deny > ask > allow > passthrough. First of each behavior type
    // wins, preserving step-order messaging for single-check cases.
    for behavior in ["deny", "ask", "allow"] {
        if let Some(decision) = decisions
            .iter()
            .find(|decision| decision.behavior() == behavior)
        {
            return decision.clone();
        }
    }

    let reason = PermissionDecisionReason::Other {
        reason: "This command requires approval".to_string(),
    };
    PermissionResult::Passthrough {
        message: create_permission_request_message(Some(&reason)),
        decision_reason: Some(reason),
        suggestions: suggestion_for_exact_command(POWERSHELL_TOOL_NAME, command),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

/// Maps to: CC `powershellPermissions.ts:1126-1130` `hasCdSubCommand`,
/// threaded into `checkPathConstraints` (:1272-1277).
///
/// CC:
/// ```js
/// const hasCdSubCommand =
///   allSubCommands.length > 1 &&
///   allSubCommands.some(({ element }) => isCwdChangingCmdlet(element.name))
/// ```
///
/// BOTH conjuncts are load-bearing. CC's `> 1` gate carries its own bug
/// number (:1123-1125, "bug #25"): a standalone `Set-Location ./subdir` is
/// not a TOCTOU risk, because no later statement resolves relative paths
/// against the stale cwd — and flagging it "forces the compound guard,
/// suppressing the per-subcommand auto-allow path", i.e. it turns every
/// single-statement `cd` into a permission prompt.
///
/// This doc comment previously claimed a DEVIATION(SAFETY) from "CC excludes
/// a no-op `Set-Location` that targets the current directory". No such
/// exclusion exists in CC 2.1.88 — :1116-1122 is an explicit note that the
/// no-op exclusion was REMOVED ("SECURITY: NO cd-to-CWD no-op exclusion. …
/// Any cd-family cmdlet in the compound sets this flag, period."), because
/// the first-non-dash-arg heuristic it relied on was fooled by colon-bound
/// parameters. Matching CC therefore means keeping every cd-family cmdlet
/// AND restoring the `> 1` gate the port had dropped.
///
/// `get_all_commands` stands in for CC's `getSubCommandsForPermissionCheck`,
/// which is not ported (see the audit doc); both enumerate one entry per
/// command element, so the arity the gate reads is the same.
fn compound_command_has_cd(parsed: &ParsedPowerShellCommand) -> bool {
    let commands = get_all_commands(parsed);
    commands.len() > 1
        && commands
            .into_iter()
            .any(|command| is_cwd_changing_cmdlet(&command.name))
}

#[derive(Default)]
struct MatchingPowerShellRules {
    matching_deny_rules: Vec<PermissionRule>,
    matching_ask_rules: Vec<PermissionRule>,
    matching_allow_rules: Vec<PermissionRule>,
}

/// Maps to: CC `tools/PowerShellTool/powershellPermissions.ts:170-333#filterRulesByContentsMatchingInput`.
fn filter_rules_by_contents_matching_input(
    command: &str,
    rules: impl IntoIterator<Item = PermissionRule>,
    behavior: PermissionBehavior,
) -> Vec<PermissionRule> {
    rules
        .into_iter()
        .filter(|rule| {
            rule.rule_value
                .rule_content
                .as_deref()
                .is_some_and(|content| powershell_rule_content_matches(content, command, behavior))
        })
        .collect()
}

/// Maps to: CC `tools/PowerShellTool/powershellPermissions.ts:338-385#matchingRulesForInput`.
fn matching_rules_for_input(
    command: &str,
    context: &ToolPermissionContext,
) -> MatchingPowerShellRules {
    let rules_for = |behavior| {
        crate::utils::permissions::permissions::get_rule_by_contents_for_tool_name(
            context,
            POWERSHELL_TOOL_NAME,
            behavior,
        )
        .into_values()
    };
    MatchingPowerShellRules {
        matching_deny_rules: filter_rules_by_contents_matching_input(
            command,
            rules_for(PermissionBehavior::Deny),
            PermissionBehavior::Deny,
        ),
        matching_ask_rules: filter_rules_by_contents_matching_input(
            command,
            rules_for(PermissionBehavior::Ask),
            PermissionBehavior::Ask,
        ),
        matching_allow_rules: filter_rules_by_contents_matching_input(
            command,
            rules_for(PermissionBehavior::Allow),
            PermissionBehavior::Allow,
        ),
    }
}

fn match_exact_or_content_rules(
    command: &str,
    context: &ToolPermissionContext,
) -> Option<PermissionResult> {
    let exact = matching_rules_for_input(command, context);
    let mut fragments = MatchingPowerShellRules::default();
    for fragment in powershell_fragments(command)
        .into_iter()
        .filter(|fragment| fragment.trim() != command.trim())
    {
        let matching = matching_rules_for_input(&fragment, context);
        fragments
            .matching_deny_rules
            .extend(matching.matching_deny_rules);
        fragments
            .matching_ask_rules
            .extend(matching.matching_ask_rules);
    }

    if let Some(rule) = exact
        .matching_deny_rules
        .first()
        .or_else(|| fragments.matching_deny_rules.first())
        .cloned()
    {
        return Some(PermissionResult::Deny {
            message: format!(
                "Permission to use {POWERSHELL_TOOL_NAME} with command {command} has been denied."
            ),
            decision_reason: PermissionDecisionReason::Rule { rule },
            tool_use_id: None,
        });
    }
    if let Some(rule) = exact
        .matching_ask_rules
        .first()
        .or_else(|| fragments.matching_ask_rules.first())
        .cloned()
    {
        return Some(PermissionResult::Ask {
            message: create_permission_request_message(None),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        });
    }
    if let Some(rule) = exact.matching_allow_rules.first().cloned() {
        return Some(PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        });
    }
    None
}

fn powershell_fragments(command: &str) -> Vec<String> {
    let collapsed = command
        .replace("`\r\n", "")
        .replace("`\n", "")
        .replace('`', "");
    collapsed
        .split([';', '|', '\n', '\r', '{', '}', '(', ')', '&'])
        .map(normalize_powershell_command)
        .filter(|fragment| !fragment.is_empty())
        .collect()
}

fn normalize_powershell_command(command: &str) -> String {
    let mut command = command.trim();
    while command.starts_with('$') {
        let Some(equals) = command.find('=') else {
            break;
        };
        command = command[equals + 1..].trim_start();
    }
    command = command
        .strip_prefix("& ")
        .or_else(|| command.strip_prefix(". "))
        .unwrap_or(command)
        .trim_start();
    let raw_name = command.split_whitespace().next().unwrap_or_default();
    let name = raw_name.trim_matches(['\'', '"']);
    if name.is_empty() {
        return String::new();
    }
    let canonical = canonical_powershell_name(name, true);
    let rest = command[raw_name.len()..].trim_start();
    if rest.is_empty() {
        canonical
    } else {
        format!("{canonical} {rest}")
    }
}

fn canonical_powershell_name(name: &str, strip_module: bool) -> String {
    let name = if strip_module {
        name.rsplit_once('\\').map_or(name, |(_, tail)| tail)
    } else {
        name
    };
    let mut lower = name.to_ascii_lowercase();
    if !lower.contains('/') && !lower.contains('\\') {
        for extension in [".exe", ".cmd", ".bat", ".com"] {
            if lower.ends_with(extension) {
                lower.truncate(lower.len() - extension.len());
                break;
            }
        }
    }
    match lower.as_str() {
        "ls" | "dir" | "gci" => "get-childitem",
        "cat" | "type" | "gc" => "get-content",
        "cd" | "sl" | "chdir" => "set-location",
        "pushd" => "push-location",
        "popd" => "pop-location",
        "pwd" | "gl" => "get-location",
        "gi" => "get-item",
        "gp" => "get-itemproperty",
        "ni" | "mkdir" | "md" => "new-item",
        "ri" | "del" | "rd" | "rmdir" | "rm" | "erase" => "remove-item",
        "mi" | "mv" | "move" => "move-item",
        "ci" | "cp" | "copy" | "cpi" => "copy-item",
        "si" => "set-item",
        "rni" | "ren" => "rename-item",
        "ps" | "gps" => "get-process",
        "kill" | "spps" => "stop-process",
        "start" | "saps" => "start-process",
        "sajb" => "start-job",
        "ipmo" => "import-module",
        "echo" | "write" => "write-output",
        "sleep" => "start-sleep",
        "help" | "man" => "get-help",
        "gcm" => "get-command",
        "gsv" => "get-service",
        "gv" => "get-variable",
        "sv" => "set-variable",
        "h" | "history" => "get-history",
        "iex" => "invoke-expression",
        "iwr" => "invoke-webrequest",
        "irm" => "invoke-restmethod",
        "icm" => "invoke-command",
        "ii" => "invoke-item",
        "nsn" => "new-pssession",
        "etsn" => "enter-pssession",
        "exsn" => "exit-pssession",
        "gsn" => "get-pssession",
        "rsn" => "remove-pssession",
        "cls" | "clear" => "clear-host",
        "select" => "select-object",
        "where" | "?" => "where-object",
        "foreach" | "%" => "foreach-object",
        "measure" => "measure-object",
        "ft" => "format-table",
        "fl" => "format-list",
        "fw" => "format-wide",
        "oh" => "out-host",
        "ogv" => "out-gridview",
        "ac" => "add-content",
        "clc" => "clear-content",
        "tee" => "tee-object",
        "epcsv" => "export-csv",
        "sp" => "set-itemproperty",
        "rp" => "remove-itemproperty",
        "cli" => "clear-item",
        "epal" => "export-alias",
        "sls" => "select-string",
        _ => return lower,
    }
    .to_string()
}

fn canonicalize_rule_command(command: &str, behavior: PermissionBehavior) -> String {
    let command = command.trim();
    let raw_name = command.split_whitespace().next().unwrap_or_default();
    let strip_module = behavior != PermissionBehavior::Allow;
    let canonical = canonical_powershell_name(raw_name.trim_matches(['\'', '"']), strip_module);
    let rest = command[raw_name.len()..].trim_start();
    if rest.is_empty() {
        canonical
    } else {
        format!("{canonical} {rest}")
    }
}

fn powershell_rule_content_matches(
    expected: &str,
    actual: &str,
    behavior: PermissionBehavior,
) -> bool {
    let actual = actual.trim();
    let compound = [";", "|", "&", "\n", "\r", "$("]
        .iter()
        .any(|operator| actual.contains(operator));
    let canonical_actual = normalize_powershell_command(actual);
    let matches = |rule: ShellPermissionRule, command: &str| match rule {
        ShellPermissionRule::Exact { command: expected } => expected.eq_ignore_ascii_case(command),
        ShellPermissionRule::Prefix { prefix } => {
            let command = command.to_ascii_lowercase();
            let prefix = prefix.to_ascii_lowercase();
            !compound && (command == prefix || command.starts_with(&format!("{prefix} ")))
        }
        ShellPermissionRule::Wildcard { pattern } => {
            !compound && match_wildcard_pattern(&pattern, command, true)
        }
    };
    if matches(parse_permission_rule(expected), actual) {
        return true;
    }
    let canonical_expected = canonicalize_rule_command(expected, behavior);
    matches(
        parse_permission_rule(&canonical_expected),
        &canonical_actual,
    )
}

/// Maps to: CC `powershellPermissions.ts:825-840` — the parse-independent
/// dangerous-removal hard-deny.
///
/// SCOPE DEVIATION: CC runs this only inside the `!parsed.valid` branch
/// (:764-874), because when the AST is available `checkPathConstraints`
/// already denies via `isDangerousRemovalPath`. This port runs it on every
/// path. The earlier claim in this comment — that a missing AST "is the
/// permanent state here" — is stale: `utils/powershell/parser.rs` spawns a
/// real pwsh and `path_validation.rs:1514-1587` carries the AST-backed
/// removal deny. The unconditional scan is now belt-and-braces: it can only
/// widen the deny set, which is the fail-safe direction, and it fires before
/// the collect-then-reduce so it cannot be masked.
fn dangerous_removal_path(command: &str) -> Option<String> {
    for fragment in powershell_fragments(command) {
        let mut words = fragment.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if resolve_to_canonical(first) != "remove-item" {
            continue;
        }
        for word in words {
            if word
                .chars()
                .next()
                .is_some_and(|first| PS_TOKENIZER_DASH_CHARS.contains(&first))
            {
                continue;
            }
            if is_dangerous_removal_raw_path(word) {
                return Some(word.to_string());
            }
        }
    }
    None
}

/// Maps to: CC `powershellPermissions.ts:1168-1257` — the git-internal and
/// `.git/` write guards.
///
/// DEVIATION(SECURITY): CC reads write-cmdlet arguments and redirection
/// targets off the PowerShell AST. Without that AST this scans the raw
/// fragment tokens instead, so it over-approximates the argument set. Both
/// guards only ever raise `ask`, so over-firing stays fail-safe.
fn git_safety_write_ask_reason(command: &str) -> Option<&'static str> {
    let fragments = powershell_fragments(command);
    let has_git_sub_command = fragments.iter().any(|fragment| {
        fragment
            .split_whitespace()
            .next()
            .is_some_and(|name| resolve_to_canonical(name) == "git")
    });

    let mut writes_to_dot_git = false;
    let mut writes_to_git_internal = false;
    for fragment in &fragments {
        let mut words = fragment.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if !GIT_SAFETY_WRITE_CMDLETS.contains(&resolve_to_canonical(first).as_str()) {
            continue;
        }
        for argument in words.flat_map(|word| word.split(',')) {
            if is_dot_git_path_ps(argument) {
                writes_to_dot_git = true;
            }
            if has_git_sub_command && is_git_internal_path_ps(argument) {
                writes_to_git_internal = true;
            }
        }
    }

    if writes_to_git_internal {
        return Some(
            "Command writes to a git-internal path (HEAD, objects/, refs/, hooks/, .git/) and runs git. This could plant a malicious hook that git then executes.",
        );
    }
    if writes_to_dot_git {
        return Some(
            "Command writes to .git/ — hooks or config planted there execute on the next git operation.",
        );
    }
    None
}

fn contains_vulnerable_unc_path(command: &str) -> bool {
    command.contains("\\\\") || command.contains("//")
}

fn contains_non_filesystem_provider(command: &str) -> bool {
    let lower = command.replace('`', "").to_ascii_lowercase();
    [
        "env:",
        "hklm:",
        "hkcu:",
        "function:",
        "alias:",
        "variable:",
        "cert:",
        "wsman:",
        "registry:",
        "registry::",
    ]
    .iter()
    .any(|provider| lower.contains(provider))
}

fn safety_ask(command: &str, reason: &str) -> PermissionResult {
    let decision_reason = PermissionDecisionReason::SafetyCheck {
        reason: reason.to_string(),
        classifier_approvable: true,
    };
    PermissionResult::Ask {
        message: reason.to_string(),
        updated_input: None,
        decision_reason: Some(decision_reason),
        suggestions: suggestion_for_exact_command(POWERSHELL_TOOL_NAME, command),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

fn ask_for_approval(command: &str, reason: &str) -> PermissionResult {
    let decision_reason = PermissionDecisionReason::Other {
        reason: reason.to_string(),
    };
    PermissionResult::Ask {
        message: create_permission_request_message(Some(&decision_reason)),
        updated_input: None,
        decision_reason: Some(decision_reason),
        suggestions: suggestion_for_exact_command(POWERSHELL_TOOL_NAME, command),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

fn create_permission_request_message(reason: Option<&PermissionDecisionReason>) -> String {
    match reason {
        Some(PermissionDecisionReason::SafetyCheck { reason, .. }) => reason.clone(),
        Some(PermissionDecisionReason::Other { reason }) => reason.clone(),
        _ => "Claude Code wants to run this PowerShell command".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};

    #[test]
    fn security_ask_is_bypass_immune_shaped() {
        let context = ToolPermissionContext::default();
        let result = powershell_tool_has_permission("iex 'Write-Host hi'", &context);
        match result {
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::SafetyCheck { .. }),
                ..
            } => {}
            other => panic!("expected SafetyCheck ask, got {other:?}"),
        }
    }

    #[test]
    fn empty_command_is_allowed_like_official() {
        assert!(matches!(
            powershell_tool_has_permission("  ", &ToolPermissionContext::default()),
            PermissionResult::Allow { .. }
        ));
    }

    #[test]
    fn compound_later_deny_beats_earlier_ask_and_resolves_aliases() {
        let mut context = ToolPermissionContext::default();
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Date:*".to_string()),
            )],
        );
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Remove-Item:*".to_string()),
            )],
        );
        assert!(matches!(
            powershell_tool_has_permission("Get-Date; rm ./secret.txt", &context),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            powershell_tool_has_permission("Write-Output ok | del ./secret.txt", &context),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn dangerous_removal_is_hard_denied() {
        for command in [
            "Remove-Item -Recurse /",
            "rm -Recurse ~",
            "Remove-Item './build/*'",
            "Get-Date; ri C:\\Windows",
        ] {
            assert!(
                matches!(
                    powershell_tool_has_permission(command, &ToolPermissionContext::default()),
                    PermissionResult::Deny {
                        decision_reason: PermissionDecisionReason::Other { ref reason },
                        ..
                    } if reason == "Removal targets a protected system path"
                ),
                "command={command:?}"
            );
        }
    }

    /// Maps to: CC `powershellPermissions.ts:747-758` — the parse-degraded
    /// allow short-circuit refuses application names, because input-side
    /// `strip_module_prefix` collapses `scripts\build.exe` to `build`, which an
    /// exact `build:*` allow would otherwise approve while PowerShell runs the
    /// local script.
    #[test]
    fn a_degraded_allow_short_circuit_refuses_application_names() {
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![
                PermissionRuleValue::new(POWERSHELL_TOOL_NAME, Some("build:*".to_string())),
                PermissionRuleValue::new(POWERSHELL_TOOL_NAME, Some("build.exe:*".to_string())),
            ],
        );

        for command in ["scripts\\build.exe --flag"] {
            // Pins the rule as actually matching, so the guard below stays
            // meaningful instead of passing for want of an allow rule.
            assert!(
                matches!(
                    match_exact_or_content_rules(command, &context),
                    Some(PermissionResult::Allow { .. })
                ),
                "rule did not match command={command:?}"
            );
            assert!(
                !matches!(
                    powershell_tool_has_permission(command, &context),
                    PermissionResult::Allow { .. }
                ),
                "command={command:?}"
            );
        }
    }

    #[test]
    fn git_internal_and_dot_git_writes_require_approval() {
        let context = ToolPermissionContext::default();
        assert!(matches!(
            powershell_tool_has_permission("Set-Content .git/hooks/pre-commit 'x'", &context),
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::SafetyCheck { ref reason, .. }),
                ..
            } if reason.starts_with("Command writes to .git/")
        ));
        assert!(matches!(
            powershell_tool_has_permission("New-Item hooks/pre-commit; git status", &context),
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::SafetyCheck { ref reason, .. }),
                ..
            } if reason.starts_with("Command writes to a git-internal path")
        ));
        assert!(!matches!(
            powershell_tool_has_permission("Set-Content ./src/main.rs 'x'", &context),
            PermissionResult::Ask { .. }
        ));
    }

    #[test]
    fn duplicate_content_matching_uses_official_later_source_metadata() {
        let mut context = ToolPermissionContext::default();
        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::Session,
        ] {
            context.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(
                    POWERSHELL_TOOL_NAME,
                    Some("Get-Date".to_string()),
                )],
            );
        }
        assert!(matches!(
            powershell_tool_has_permission("Get-Date", &context),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                ..
            } if rule.source == PermissionRuleSource::Session
        ));
    }

    /// Maps to: CC `powershellPermissions.ts:694-711` + `:902-907`. A matching
    /// ask RULE is deferred into `decisions[]`, never returned early, so the
    /// non-rule denies that run after it still fire. CC names the bug at :695:
    /// "Previously this early-returned before sub-command deny checks ran".
    ///
    /// `compound_later_deny_beats_earlier_ask_and_resolves_aliases` above only
    /// covers rule-vs-rule, which `match_exact_or_content_rules` already
    /// ordered internally; this covers rule-ask vs. the deny sources that live
    /// past the return — the dangerous-removal hard-deny (:825-840) and path
    /// constraints (:1271-1276).
    #[test]
    fn a_matching_ask_rule_does_not_mask_a_later_non_rule_deny() {
        let mut context = ToolPermissionContext::default();
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Date:*".to_string()),
            )],
        );
        // Pins the ask rule as matching THE COMPOUND COMMAND, not just the
        // bare one. Without this the test would pass vacuously against the
        // old early-return shape, because the matcher would be returning
        // passthrough and there would be no ask to mask the deny with.
        assert!(
            matches!(
                match_exact_or_content_rules("Get-Date; Remove-Item -Recurse /", &context),
                Some(PermissionResult::Ask { .. })
            ),
            "ask rule did not match the compound command — the guard below \
             would be vacuous"
        );
        // No deny RULE here — the deny comes from the dangerous-removal guard,
        // which sits after the point the old code returned from.
        assert!(matches!(
            powershell_tool_has_permission("Get-Date; Remove-Item -Recurse /", &context),
            PermissionResult::Deny {
                decision_reason: PermissionDecisionReason::Other { ref reason },
                ..
            } if reason == "Removal targets a protected system path"
        ));
        // With nothing to deny, the deferred ask is still what comes back, and
        // it keeps its rule attribution (CC :903-906 pushes it first, so it
        // wins among asks).
        assert!(matches!(
            powershell_tool_has_permission("Get-Date", &context),
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::Rule { .. }),
                ..
            }
        ));
    }

    /// Maps to: CC `powershellPermissions.ts:750-757` — the parse-degraded
    /// exact-allow short-circuit is guarded by `preParseAskDecision === null`,
    /// which covers a deferred prefix ask as well as the raw UNC ask. CC's
    /// comment at :725-730 records that dropping the ask half "silently
    /// overrid[es] the ask with allow".
    #[test]
    fn a_pending_ask_rule_blocks_the_degraded_allow_short_circuit() {
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Date".to_string()),
            )],
        );
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Date".to_string()),
            )],
        );
        // Deny > ask > allow inside the rule matcher means the ask is what the
        // pre-parse step carries; the allow must not overtake it on either the
        // parse-succeeded or the parse-degraded path.
        assert!(matches!(
            powershell_tool_has_permission("Get-Date", &context),
            PermissionResult::Ask { .. }
        ));
    }

    /// Maps to: CC `powershellPermissions.ts:1123-1130`. A STANDALONE
    /// cwd-changing cmdlet must not set the compound-cd flag — CC gates it on
    /// `allSubCommands.length > 1` and calls the missing gate "bug #25",
    /// because the flag forces `checkPathConstraints` to ask for any statement
    /// with path operations.
    #[test]
    fn compound_cd_flag_needs_more_than_one_sub_command() {
        use crate::utils::powershell::parser::{
            ParsedCommandElement, ParsedStatement, StatementType,
        };

        // Built from AST fixtures, not `parse_powershell_command`: the parser
        // shells out to pwsh, which is absent on the POSIX dev hosts, and a
        // degraded parse carries zero commands — both sides would then be
        // vacuously false and the regression would pass unnoticed.
        fn element(name: &str) -> ParsedCommandElement {
            ParsedCommandElement {
                name: name.to_string(),
                name_type: Some(CommandNameType::Cmdlet),
                element_type: None,
                args: Vec::new(),
                text: name.to_string(),
                element_types: None,
                children: None,
                redirections: None,
            }
        }
        fn parsed(names: &[&str]) -> ParsedPowerShellCommand {
            ParsedPowerShellCommand {
                valid: true,
                errors: Vec::new(),
                statements: vec![ParsedStatement {
                    statement_type: StatementType::PipelineAst,
                    commands: names.iter().copied().map(element).collect(),
                    redirections: Vec::new(),
                    text: names.join("; "),
                    nested_commands: None,
                    security_patterns: None,
                }],
                variables: Vec::new(),
                has_stop_parsing: false,
                original_command: names.join("; "),
                type_literals: Vec::new(),
                has_using_statements: false,
                has_script_requirements: false,
            }
        }

        assert!(
            !compound_command_has_cd(&parsed(&["Set-Location"])),
            "a standalone Set-Location is not a TOCTOU risk (CC :1123-1125)"
        );
        assert!(
            !compound_command_has_cd(&parsed(&["cd"])),
            "the alias form is standalone too"
        );
        assert!(
            compound_command_has_cd(&parsed(&["Set-Location", "Get-ChildItem"])),
            "a cd paired with another sub-command still sets the flag"
        );
        assert!(
            !compound_command_has_cd(&parsed(&["Get-ChildItem", "Select-Object"])),
            "arity alone must not set the flag"
        );
    }

    #[test]
    fn exact_allow_rule_short_circuits() {
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                POWERSHELL_TOOL_NAME,
                Some("Get-Date".to_string()),
            )],
        );
        assert!(matches!(
            powershell_tool_has_permission("Get-Date", &context),
            PermissionResult::Allow { .. }
        ));
    }
}
