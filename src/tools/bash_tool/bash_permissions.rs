//! Maps to: CC `tools/BashTool/bashPermissions.ts`.
//!
//! This module owns Bash permission rule normalization, command permission
//! checks, and the safe-disabled classifier seams used by `useCanUseTool`.
//! Full tree-sitter parsing and credentialed classifier RPCs remain in broader
//! integration slices.

use regex::Regex;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionRule, PermissionUpdate};
use crate::utils::bash::commands::split_command_deprecated;
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use crate::utils::permissions::permissions::get_rule_by_contents_for_tool_name;
use crate::utils::permissions::shell_rule_matching::{
    ShellPermissionRule, match_wildcard_pattern as shared_match_wildcard_pattern,
    parse_permission_rule, permission_rule_extract_prefix as shared_permission_rule_extract_prefix,
    suggestion_for_exact_command as shared_suggestion_for_exact_command,
    suggestion_for_prefix as shared_suggestion_for_prefix,
};

/// Maps to: CC `MAX_SUBCOMMANDS_FOR_SECURITY_CHECK`.
pub const MAX_SUBCOMMANDS_FOR_SECURITY_CHECK: usize = 50;
/// Maps to: CC `MAX_SUGGESTED_RULES_FOR_COMPOUND`.
pub const MAX_SUGGESTED_RULES_FOR_COMPOUND: usize = 5;

static SPECULATIVE_CLASSIFIER_CHECKS: LazyLock<
    Mutex<HashMap<String, crate::utils::permissions::bash_classifier::ClassifierResult>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

static ENV_VAR_ASSIGN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*=").expect("valid env assignment regex"));
static SIMPLE_SUBCOMMAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(-[a-z0-9]+)*$").expect("valid subcommand regex"));
static SAFE_ENV_VAR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)=([A-Za-z0-9_./:-]+)[ \t]+")
        .expect("valid safe env var regex")
});
static STRIP_ALL_ENV_VAR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^([A-Za-z_][A-Za-z0-9_]*(?:\[[^\]]*\])?)\+?=(?:'[^'\n\r]*'|"(?:\\.|[^"$`\\\n\r])*"|\\.|[^ \t\n\r$`;|&()<>\\'"])*[ \t]+"#,
    )
    .expect("valid broad env var regex")
});
static TIMEOUT_WRAPPER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^timeout[ \t]+(?:(?:--(?:foreground|preserve-status|verbose)|--(?:kill-after|signal)=[A-Za-z0-9_.+-]+|--(?:kill-after|signal)[ \t]+[A-Za-z0-9_.+-]+|-v|-[ks][ \t]+[A-Za-z0-9_.+-]+|-[ks][A-Za-z0-9_.+-]+)[ \t]+)*(?:--[ \t]+)?\d+(?:\.\d+)?[smhd]?[ \t]+")
        .expect("valid timeout wrapper regex")
});
static TIME_WRAPPER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^time[ \t]+(?:--[ \t]+)?").expect("valid time regex"));
static NICE_WRAPPER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^nice(?:[ \t]+-n[ \t]+-?\d+|[ \t]+-\d+)?[ \t]+(?:--[ \t]+)?")
        .expect("valid nice regex")
});
static STDBUF_WRAPPER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^stdbuf(?:[ \t]+-[ioe][LN0-9]+)+[ \t]+(?:--[ \t]+)?").expect("valid stdbuf regex")
});
static NOHUP_WRAPPER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^nohup[ \t]+(?:--[ \t]+)?").expect("valid nohup regex"));
static TIMEOUT_FLAG_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.+-]+$").expect("valid timeout value regex"));
static TIMEOUT_DURATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(?:\.\d+)?[smhd]?$").expect("valid timeout duration regex"));
static SHORT_TIMEOUT_FLAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-[ks][A-Za-z0-9_.+-]+$").expect("valid flag regex"));

/// Maps to CC `BINARY_HIJACK_VARS`.
pub static BINARY_HIJACK_VARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(LD_|DYLD_|PATH$)").expect("valid hijack regex"));

const SAFE_ENV_VARS: &[&str] = &[
    "GOEXPERIMENT",
    "GOOS",
    "GOARCH",
    "CGO_ENABLED",
    "GO111MODULE",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "NODE_ENV",
    "PYTHONUNBUFFERED",
    "PYTHONDONTWRITEBYTECODE",
    "PYTEST_DISABLE_PLUGIN_AUTOLOAD",
    "PYTEST_DEBUG",
    "ANTHROPIC_API_KEY",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_TIME",
    "CHARSET",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "TZ",
    "LS_COLORS",
    "LSCOLORS",
    "GREP_COLOR",
    "GREP_COLORS",
    "GCC_COLORS",
    "TIME_STYLE",
    "BLOCK_SIZE",
    "BLOCKSIZE",
];

const ANT_ONLY_SAFE_ENV_VARS: &[&str] = &[
    "KUBECONFIG",
    "DOCKER_HOST",
    "AWS_PROFILE",
    "CLOUDSDK_CORE_PROJECT",
    "CLUSTER",
    "COO_CLUSTER",
    "COO_CLUSTER_NAME",
    "COO_NAMESPACE",
    "COO_LAUNCH_YAML_DRY_RUN",
    "SKIP_NODE_VERSION_CHECK",
    "EXPECTTEST_ACCEPT",
    "CI",
    "GIT_LFS_SKIP_SMUDGE",
    "CUDA_VISIBLE_DEVICES",
    "JAX_PLATFORMS",
    "COLUMNS",
    "TMUX",
    "POSTGRESQL_VERSION",
    "FIRESTORE_EMULATOR_HOST",
    "HARNESS_QUIET",
    "TEST_CROSSCHECK_LISTS_MATCH_UPDATE",
    "DBT_PER_DEVELOPER_ENVIRONMENTS",
    "STATSIG_FORD_DB_CHECKS",
    "ANT_ENVIRONMENT",
    "ANT_SERVICE",
    "MONOREPO_ROOT_DIR",
    "PYENV_VERSION",
    "PGPASSWORD",
    "GH_TOKEN",
    "GROWTHBOOK_API_KEY",
];

const BARE_SHELL_PREFIXES: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "csh",
    "tcsh",
    "ksh",
    "dash",
    "cmd",
    "powershell",
    "pwsh",
    "env",
    "xargs",
    "nice",
    "stdbuf",
    "nohup",
    "timeout",
    "time",
    "sudo",
    "doas",
    "pkexec",
];

/// Maps to: CC `getSimpleCommandPrefix(command)`.
pub fn get_simple_command_prefix(command: &str) -> Option<String> {
    let tokens = command
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }

    let mut index = 0usize;
    while index < tokens.len() && ENV_VAR_ASSIGN_RE.is_match(tokens[index]) {
        let var_name = tokens[index].split('=').next().unwrap_or_default();
        if !is_safe_env_var(var_name) {
            return None;
        }
        index += 1;
    }

    let remaining = &tokens[index..];
    if remaining.len() < 2 {
        return None;
    }
    let subcommand = remaining[1];
    SIMPLE_SUBCOMMAND_RE
        .is_match(subcommand)
        .then(|| format!("{} {}", remaining[0], remaining[1]))
}

/// Maps to: CC `getFirstWordPrefix(command)`.
pub fn get_first_word_prefix(command: &str) -> Option<String> {
    let tokens = command
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0usize;
    while index < tokens.len() && ENV_VAR_ASSIGN_RE.is_match(tokens[index]) {
        let var_name = tokens[index].split('=').next().unwrap_or_default();
        if !is_safe_env_var(var_name) {
            return None;
        }
        index += 1;
    }
    let command = *tokens.get(index)?;
    (SIMPLE_SUBCOMMAND_RE.is_match(command) && !BARE_SHELL_PREFIXES.contains(&command))
        .then(|| command.to_string())
}

/// Maps to: CC internal `suggestionForExactCommand(command)`.
pub fn suggestion_for_exact_command(command: &str) -> Vec<PermissionUpdate> {
    if let Some(prefix) = extract_prefix_before_heredoc(command) {
        return shared_suggestion_for_prefix(super::tool_name::BASH_TOOL_NAME, &prefix);
    }
    if command.contains('\n') {
        if let Some(first_line) = command
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return shared_suggestion_for_prefix(super::tool_name::BASH_TOOL_NAME, first_line);
        }
    }
    if let Some(prefix) = get_simple_command_prefix(command) {
        return shared_suggestion_for_prefix(super::tool_name::BASH_TOOL_NAME, &prefix);
    }
    shared_suggestion_for_exact_command(super::tool_name::BASH_TOOL_NAME, command)
}

/// Maps to: CC internal `suggestionForPrefix(prefix)`.
pub fn suggestion_for_prefix(prefix: &str) -> Vec<PermissionUpdate> {
    shared_suggestion_for_prefix(super::tool_name::BASH_TOOL_NAME, prefix)
}

/// Maps to: CC internal `extractPrefixBeforeHeredoc(command)`.
pub fn extract_prefix_before_heredoc(command: &str) -> Option<String> {
    let idx = command.find("<<")?;
    if idx == 0 {
        return None;
    }
    let before = command[..idx].trim();
    if before.is_empty() {
        return None;
    }
    if let Some(prefix) = get_simple_command_prefix(before) {
        return Some(prefix);
    }
    let tokens = before
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0usize;
    while index < tokens.len() && ENV_VAR_ASSIGN_RE.is_match(tokens[index]) {
        let var_name = tokens[index].split('=').next().unwrap_or_default();
        if !is_safe_env_var(var_name) {
            return None;
        }
        index += 1;
    }
    (index < tokens.len()).then(|| {
        tokens[index..tokens.len().min(index + 2)]
            .join(" ")
            .to_string()
    })
}

/// Maps to: CC `permissionRuleExtractPrefix` re-export.
pub fn permission_rule_extract_prefix(permission_rule: &str) -> Option<&str> {
    shared_permission_rule_extract_prefix(permission_rule)
}

/// Maps to: CC `matchWildcardPattern(pattern, command)`.
pub fn match_wildcard_pattern(pattern: &str, command: &str) -> bool {
    shared_match_wildcard_pattern(pattern, command, false)
}

/// Maps to: CC `bashPermissionRule(permissionRule)`.
pub fn bash_permission_rule(permission_rule: &str) -> ShellPermissionRule {
    parse_permission_rule(permission_rule)
}

/// Maps to: CC `stripSafeWrappers(command)`.
pub fn strip_safe_wrappers(command: &str) -> String {
    let mut stripped = command.to_string();
    let mut previous = String::new();

    while stripped != previous {
        previous = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        if let Some(captures) = SAFE_ENV_VAR_PATTERN.captures(&stripped) {
            let var_name = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
            if is_safe_env_var(var_name) {
                let end = captures.get(0).map(|m| m.end()).unwrap_or(0);
                stripped = stripped[end..].to_string();
            }
        }
    }

    previous.clear();
    while stripped != previous {
        previous = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        for regex in [
            &*TIMEOUT_WRAPPER_RE,
            &*TIME_WRAPPER_RE,
            &*NICE_WRAPPER_RE,
            &*STDBUF_WRAPPER_RE,
            &*NOHUP_WRAPPER_RE,
        ] {
            if regex.is_match(&stripped) {
                stripped = regex.replace(&stripped, "").to_string();
            }
        }
    }

    stripped.trim().to_string()
}

/// Maps to: CC `stripWrappersFromArgv(argv)`.
pub fn strip_wrappers_from_argv(argv: &[String]) -> Vec<String> {
    let mut args = argv.to_vec();
    loop {
        match args.first().map(String::as_str) {
            Some("time") | Some("nohup") => {
                let cut = if args.get(1).is_some_and(|arg| arg == "--") {
                    2
                } else {
                    1
                };
                args = args.into_iter().skip(cut).collect();
            }
            Some("timeout") => {
                let index = skip_timeout_flags(&args);
                if index < 0 {
                    return args;
                }
                let index = index as usize;
                if !args
                    .get(index)
                    .is_some_and(|arg| TIMEOUT_DURATION_RE.is_match(arg))
                {
                    return args;
                }
                args = args.into_iter().skip(index + 1).collect();
            }
            Some("nice")
                if args.get(1).is_some_and(|arg| arg == "-n")
                    && args.get(2).is_some_and(|arg| parse_i64(arg).is_some()) =>
            {
                let cut = if args.get(3).is_some_and(|arg| arg == "--") {
                    4
                } else {
                    3
                };
                args = args.into_iter().skip(cut).collect();
            }
            _ => return args,
        }
    }
}

/// Maps to: CC `stripAllLeadingEnvVars(command, blocklist)`.
pub fn strip_all_leading_env_vars(command: &str, blocklist: Option<&Regex>) -> String {
    let mut stripped = command.to_string();
    let mut previous = String::new();
    while stripped != previous {
        previous = stripped.clone();
        stripped = strip_comment_lines(&stripped);
        let Some(captures) = STRIP_ALL_ENV_VAR_PATTERN.captures(&stripped) else {
            continue;
        };
        let var_name = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
        if blocklist.is_some_and(|regex| regex.is_match(var_name)) {
            break;
        }
        let end = captures.get(0).map(|m| m.end()).unwrap_or(0);
        stripped = stripped[end..].to_string();
    }
    stripped.trim().to_string()
}

/// Maps to: CC `isNormalizedGitCommand(command)`.
pub fn is_normalized_git_command(command: &str) -> bool {
    if command == "git" || command.starts_with("git ") {
        return true;
    }
    let stripped = strip_safe_wrappers(command);
    if let Ok(tokens) = crate::utils::bash::shell_quote::try_parse_shell_command(&stripped) {
        if let Some(crate::utils::bash::shell_quote::ParseEntry::String(first)) = tokens.first() {
            if first == "git" {
                return true;
            }
            if first == "xargs"
                && tokens.iter().any(|token| {
                    matches!(token, crate::utils::bash::shell_quote::ParseEntry::String(value) if value == "git")
                })
            {
                return true;
            }
            return false;
        }
    }
    stripped == "git" || stripped.starts_with("git ")
}

/// Maps to: CC `isNormalizedCdCommand(command)`.
pub fn is_normalized_cd_command(command: &str) -> bool {
    let stripped = strip_safe_wrappers(command);
    if let Ok(tokens) = crate::utils::bash::shell_quote::try_parse_shell_command(&stripped) {
        if let Some(crate::utils::bash::shell_quote::ParseEntry::String(first)) = tokens.first() {
            return matches!(first.as_str(), "cd" | "pushd" | "popd");
        }
    }
    matches!(
        stripped.split_whitespace().next(),
        Some("cd" | "pushd" | "popd")
    )
}

/// Maps to: CC `commandHasAnyCd(command)`.
pub fn command_has_any_cd(command: &str) -> bool {
    split_command_deprecated(command)
        .into_iter()
        .any(|subcommand| is_normalized_cd_command(subcommand.trim()))
}

fn skip_timeout_flags(args: &[String]) -> isize {
    let mut index = 1usize;
    while index < args.len() {
        let arg = &args[index];
        let next = args.get(index + 1);
        if matches!(
            arg.as_str(),
            "--foreground" | "--preserve-status" | "--verbose"
        ) || Regex::new(r"^--(?:kill-after|signal)=[A-Za-z0-9_.+-]+$")
            .expect("valid timeout long regex")
            .is_match(arg)
        {
            index += 1;
        } else if matches!(arg.as_str(), "--kill-after" | "--signal")
            && next.is_some_and(|value| TIMEOUT_FLAG_VALUE_RE.is_match(value))
        {
            index += 2;
        } else if arg == "--" {
            index += 1;
            break;
        } else if arg.starts_with("--") {
            return -1;
        } else if arg == "-v" {
            index += 1;
        } else if matches!(arg.as_str(), "-k" | "-s")
            && next.is_some_and(|value| TIMEOUT_FLAG_VALUE_RE.is_match(value))
        {
            index += 2;
        } else if SHORT_TIMEOUT_FLAG_RE.is_match(arg) {
            index += 1;
        } else if arg.starts_with('-') {
            return -1;
        } else {
            break;
        }
    }
    index as isize
}

fn is_safe_env_var(var_name: &str) -> bool {
    SAFE_ENV_VARS.contains(&var_name)
        || (crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Permissions,
        ) && ANT_ONLY_SAFE_ENV_VARS.contains(&var_name))
}

fn strip_comment_lines(command: &str) -> String {
    let non_comment_lines = command
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .collect::<Vec<_>>();
    if non_comment_lines.is_empty() {
        command.to_string()
    } else {
        non_comment_lines.join("\n")
    }
}

fn parse_i64(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}

pub(super) fn has_obscured_command_names(command: &str) -> bool {
    if command.contains("$(") || command.contains("<(") || command.contains(">(") {
        return true;
    }
    crate::utils::bash::commands::split_command_deprecated(command)
        .into_iter()
        .any(|part| {
            let name = part.split_ascii_whitespace().next().unwrap_or_default();
            name.chars()
                .any(|ch| matches!(ch, '\'' | '"' | '\\' | '$' | '`'))
                || part.contains("$(")
                || part.contains("<(")
                || part.contains(">(")
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BashRuleMatchMode {
    Exact,
    Prefix,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchingBashRules {
    pub matching_deny_rules: Vec<PermissionRule>,
    pub matching_ask_rules: Vec<PermissionRule>,
    pub matching_allow_rules: Vec<PermissionRule>,
}

/// Maps to: CC `filterRulesByContentsMatchingInput(...)`.
pub fn filter_rules_by_contents_matching_input(
    command: &str,
    rules: &indexmap::IndexMap<String, PermissionRule>,
    match_mode: BashRuleMatchMode,
    strip_all_env_vars: bool,
    skip_compound_check: bool,
) -> Vec<PermissionRule> {
    let command = command.trim();
    let command_without_redirections =
        super::bash_command_helpers::build_segment_without_redirections(command);
    let mut commands_to_try = match match_mode {
        BashRuleMatchMode::Exact => vec![command.to_string(), command_without_redirections],
        BashRuleMatchMode::Prefix => vec![command_without_redirections],
    };
    let initial = commands_to_try.clone();
    for candidate in initial {
        let stripped = strip_safe_wrappers(&candidate);
        if stripped != candidate && !commands_to_try.contains(&stripped) {
            commands_to_try.push(stripped);
        }
    }

    if strip_all_env_vars {
        let mut start_index = 0usize;
        while start_index < commands_to_try.len() {
            let end = commands_to_try.len();
            for index in start_index..end {
                let candidate = commands_to_try[index].clone();
                let env_stripped = strip_all_leading_env_vars(&candidate, None);
                if !commands_to_try.contains(&env_stripped) {
                    commands_to_try.push(env_stripped);
                }
                let wrapper_stripped = strip_safe_wrappers(&candidate);
                if !commands_to_try.contains(&wrapper_stripped) {
                    commands_to_try.push(wrapper_stripped);
                }
            }
            start_index = end;
        }
    }

    rules
        .iter()
        .filter_map(|(rule_content, rule)| {
            let bash_rule = bash_permission_rule(rule_content);
            let matched = commands_to_try.iter().any(|candidate| {
                bash_rule_matches_candidate(&bash_rule, candidate, match_mode, skip_compound_check)
            });
            matched.then(|| rule.clone())
        })
        .collect()
}

fn bash_rule_matches_candidate(
    bash_rule: &ShellPermissionRule,
    candidate: &str,
    match_mode: BashRuleMatchMode,
    skip_compound_check: bool,
) -> bool {
    match bash_rule {
        ShellPermissionRule::Exact { command } => command == candidate,
        ShellPermissionRule::Prefix { prefix } => match match_mode {
            BashRuleMatchMode::Exact => prefix == candidate,
            BashRuleMatchMode::Prefix => {
                if !skip_compound_check && split_command_deprecated(candidate).len() > 1 {
                    return false;
                }
                candidate == prefix
                    || candidate.starts_with(&format!("{prefix} "))
                    || candidate == &format!("xargs {prefix}")
                    || candidate.starts_with(&format!("xargs {prefix} "))
            }
        },
        ShellPermissionRule::Wildcard { pattern } => {
            if match_mode == BashRuleMatchMode::Exact {
                return false;
            }
            if !skip_compound_check && split_command_deprecated(candidate).len() > 1 {
                return false;
            }
            match_wildcard_pattern(pattern, candidate)
        }
    }
}

/// Maps to: CC local `matchingRulesForInput(...)`.
pub fn matching_rules_for_input(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    match_mode: BashRuleMatchMode,
    skip_compound_check: bool,
) -> MatchingBashRules {
    MatchingBashRules {
        matching_deny_rules: filter_rules_by_contents_matching_input(
            command,
            &get_rule_by_contents_for_tool_name(
                tool_permission_context,
                super::tool_name::BASH_TOOL_NAME,
                crate::types::permissions::PermissionBehavior::Deny,
            ),
            match_mode,
            true,
            true,
        ),
        matching_ask_rules: filter_rules_by_contents_matching_input(
            command,
            &get_rule_by_contents_for_tool_name(
                tool_permission_context,
                super::tool_name::BASH_TOOL_NAME,
                crate::types::permissions::PermissionBehavior::Ask,
            ),
            match_mode,
            true,
            true,
        ),
        matching_allow_rules: filter_rules_by_contents_matching_input(
            command,
            &get_rule_by_contents_for_tool_name(
                tool_permission_context,
                super::tool_name::BASH_TOOL_NAME,
                crate::types::permissions::PermissionBehavior::Allow,
            ),
            match_mode,
            false,
            skip_compound_check,
        ),
    }
}

/// Maps to: CC `bashToolCheckExactMatchPermission(...)`.
pub fn bash_tool_check_exact_match_permission(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    let command = command.trim();
    let matching = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Exact,
        false,
    );
    if let Some(rule) = matching.matching_deny_rules.first() {
        return PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
            tool_use_id: None,
        };
    }
    if let Some(rule) = matching.matching_ask_rules.first() {
        return PermissionResult::Ask {
            message: create_permission_request_message(None),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule: rule.clone() }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }
    if let Some(rule) = matching.matching_allow_rules.first() {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule: rule.clone() }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }
    let reason = PermissionDecisionReason::Other {
        reason: "This command requires approval".to_string(),
    };
    PermissionResult::Passthrough {
        message: create_permission_request_message(Some(&reason)),
        decision_reason: Some(reason),
        suggestions: suggestion_for_exact_command(command),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

/// Maps to CC `checkEarlyExitDeny(...)` and `checkSemanticsDeny(...)`.
pub(super) fn ast_security_result(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    reason: &str,
    subcommands: &[crate::utils::bash::ast::SimpleCommand],
) -> PermissionResult {
    let exact = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if !matches!(exact, PermissionResult::Passthrough { .. }) {
        return exact;
    }
    let full = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Prefix,
        false,
    );
    let deny = full.matching_deny_rules.first().cloned().or_else(|| {
        subcommands.iter().find_map(|subcommand| {
            matching_rules_for_input(
                &subcommand.text,
                tool_permission_context,
                BashRuleMatchMode::Prefix,
                false,
            )
            .matching_deny_rules
            .first()
            .cloned()
        })
    });
    if let Some(rule) = deny {
        return PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule },
            tool_use_id: None,
        };
    }
    let decision_reason = PermissionDecisionReason::Other {
        reason: reason.to_string(),
    };
    PermissionResult::Ask {
        message: create_permission_request_message(Some(&decision_reason)),
        updated_input: None,
        decision_reason: Some(decision_reason),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: build_pending_classifier_check(command, tool_permission_context),
        content_blocks: Vec::new(),
    }
}

/// Maps to: CC `bashToolCheckPermission(...)` legacy-string path.
fn bash_tool_check_permission(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    compound_command_has_cd: bool,
    cwd: &std::path::Path,
) -> PermissionResult {
    let command = command.trim();
    let exact_match = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if matches!(
        exact_match,
        PermissionResult::Deny { .. } | PermissionResult::Ask { .. }
    ) {
        return exact_match;
    }

    let matching = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Prefix,
        false,
    );
    if let Some(rule) = matching.matching_deny_rules.first() {
        return PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
            tool_use_id: None,
        };
    }
    if let Some(rule) = matching.matching_ask_rules.first() {
        return PermissionResult::Ask {
            message: create_permission_request_message(None),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule: rule.clone() }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let cwd_string = cwd.display().to_string();
    let path_result = super::path_validation::check_path_constraints(
        command,
        &cwd_string,
        tool_permission_context,
        compound_command_has_cd,
    );
    if !matches!(path_result, PermissionResult::Passthrough { .. }) {
        return path_result;
    }

    if matches!(exact_match, PermissionResult::Allow { .. }) {
        return exact_match;
    }
    if let Some(rule) = matching.matching_allow_rules.first() {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule: rule.clone() }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    let sed_result = super::sed_validation::check_sed_constraints(command, tool_permission_context);
    if !matches!(sed_result, PermissionResult::Passthrough { .. }) {
        return sed_result;
    }

    let mode_result =
        super::mode_validation::check_permission_mode(command, tool_permission_context);
    if !matches!(mode_result, PermissionResult::Passthrough { .. }) {
        return mode_result;
    }

    if super::read_only_validation::check_read_only_constraints(command, cwd) {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Read-only command is allowed".to_string(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    let reason = PermissionDecisionReason::Other {
        reason: "This command requires approval".to_string(),
    };
    PermissionResult::Passthrough {
        message: create_permission_request_message(Some(&reason)),
        decision_reason: Some(reason),
        suggestions: suggestion_for_exact_command(command),
        blocked_path: None,
        pending_classifier_check: build_pending_classifier_check(command, tool_permission_context),
    }
}

/// Maps to: CC `checkCommandAndSuggestRules(...)`.
fn check_command_and_suggest_rules(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    command_prefix: Option<&str>,
    compound_command_has_cd: bool,
    cwd: &std::path::Path,
) -> PermissionResult {
    let exact_match = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if matches!(
        exact_match,
        PermissionResult::Deny { .. } | PermissionResult::Ask { .. }
    ) {
        return exact_match;
    }

    let permission_result = bash_tool_check_permission(
        command,
        tool_permission_context,
        compound_command_has_cd,
        cwd,
    );
    if matches!(
        permission_result,
        PermissionResult::Deny { .. } | PermissionResult::Ask { .. }
    ) {
        return permission_result;
    }

    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_COMMAND_INJECTION_CHECK")
            .as_deref(),
    ) {
        let safety_result = super::bash_security::bash_command_is_safe_deprecated(command);
        if !matches!(safety_result, PermissionResult::Passthrough { .. }) {
            let reason = match safety_result {
                PermissionResult::Ask {
                    decision_reason: Some(PermissionDecisionReason::SafetyCheck { reason, .. }),
                    ..
                } => reason,
                PermissionResult::Ask { message, .. } => message,
                _ => "This command contains patterns that could pose security risks and requires approval"
                    .to_string(),
            };
            let reason = PermissionDecisionReason::SafetyCheck {
                reason,
                classifier_approvable: true,
            };
            return PermissionResult::Ask {
                message: create_permission_request_message(Some(&reason)),
                updated_input: None,
                decision_reason: Some(reason),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    if matches!(permission_result, PermissionResult::Allow { .. }) {
        return permission_result;
    }

    let suggested_updates = command_prefix
        .map(suggestion_for_prefix)
        .unwrap_or_else(|| suggestion_for_exact_command(command));
    match permission_result {
        PermissionResult::Passthrough {
            message,
            decision_reason,
            blocked_path,
            pending_classifier_check,
            ..
        } => PermissionResult::Passthrough {
            message,
            decision_reason,
            suggestions: suggested_updates,
            blocked_path,
            pending_classifier_check,
        },
        other => other,
    }
}

/// Maps to CC `checkSandboxAutoAllow(...)`: explicit deny wins across the
/// full command and every compound segment, ask is next, otherwise OS-sandbox
/// execution is auto-allowed.
pub fn check_sandbox_auto_allow(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    check_sandbox_auto_allow_with_subcommands(command, tool_permission_context, None)
}

/// AST-authoritative sandbox auto-allow variant. Maps to the same CC
/// `checkSandboxAutoAllow`, with `splitCommand` replaced by already-verified
/// command spans so control-flow wrappers cannot hide an explicit rule.
pub fn check_sandbox_auto_allow_from_ast(
    command: &str,
    commands: &[crate::utils::bash::ast::SimpleCommand],
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    let subcommands = commands
        .iter()
        .map(|command| command.text.as_str())
        .collect::<Vec<_>>();
    check_sandbox_auto_allow_with_subcommands(command, tool_permission_context, Some(&subcommands))
}

fn check_sandbox_auto_allow_with_subcommands(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    verified_subcommands: Option<&[&str]>,
) -> PermissionResult {
    let command = command.trim();
    let full = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Prefix,
        false,
    );
    if let Some(rule) = full.matching_deny_rules.first() {
        return PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
            tool_use_id: None,
        };
    }

    let mut first_ask = None;
    let fallback_subcommands;
    let subcommands = if let Some(verified) = verified_subcommands {
        verified.to_vec()
    } else {
        fallback_subcommands = split_command_deprecated(command);
        fallback_subcommands
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|subcommand| !subcommand.is_empty())
            .collect::<Vec<_>>()
    };
    if subcommands.len() > 1 {
        for subcommand in subcommands {
            let matching = matching_rules_for_input(
                subcommand,
                tool_permission_context,
                BashRuleMatchMode::Prefix,
                false,
            );
            if let Some(rule) = matching.matching_deny_rules.first() {
                return PermissionResult::Deny {
                    message: format!(
                        "Permission to use Bash with command {command} has been denied."
                    ),
                    decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
                    tool_use_id: None,
                };
            }
            first_ask = first_ask.or_else(|| matching.matching_ask_rules.first().cloned());
        }
    }
    first_ask = first_ask.or_else(|| full.matching_ask_rules.first().cloned());
    if let Some(rule) = first_ask {
        return PermissionResult::Ask {
            message: create_permission_request_message(None),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    PermissionResult::Allow {
        updated_input: Some(serde_json::json!({ "command": command })),
        user_modified: None,
        decision_reason: Some(PermissionDecisionReason::Other {
            reason: "Auto-allowed with sandbox (autoAllowBashIfSandboxed enabled)".to_string(),
        }),
        tool_use_id: None,
        accept_feedback: None,
        content_blocks: Vec::new(),
    }
}

/// Maps to CC `bashToolHasPermission(...)` after a successful native AST
/// projection (`bashPermissions.ts:1798-2480`). The parser has already proved
/// each argv trustworthy, so this path checks every extracted command without
/// routing the original control-flow source back through the legacy splitter.
pub fn bash_tool_has_permission_from_ast(
    command: &str,
    commands: &[crate::utils::bash::ast::SimpleCommand],
    tool_permission_context: &ToolPermissionContext,
    cwd: &std::path::Path,
) -> PermissionResult {
    use std::collections::BTreeMap;

    let command = command.trim();
    let exact = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if matches!(
        exact,
        PermissionResult::Deny { .. } | PermissionResult::Ask { .. }
    ) {
        return exact;
    }

    let full_matching = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Prefix,
        false,
    );
    if let Some(rule) = full_matching.matching_deny_rules.first() {
        return PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
            tool_use_id: None,
        };
    }
    if let Some(rule) = full_matching.matching_ask_rules.first() {
        return PermissionResult::Ask {
            message: create_permission_request_message(None),
            updated_input: None,
            decision_reason: Some(PermissionDecisionReason::Rule { rule: rule.clone() }),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        };
    }

    let normalized = commands
        .iter()
        .map(|command| strip_wrappers_from_argv(&command.argv))
        .collect::<Vec<_>>();
    let cd_count = normalized
        .iter()
        .filter(|argv| {
            argv.first()
                .is_some_and(|name| matches!(name.as_str(), "cd" | "pushd" | "popd"))
        })
        .count();
    if cd_count > 1 {
        return ask_without_suggestions(
            "Multiple directory changes in one command require approval for clarity",
        );
    }
    let compound_command_has_cd = cd_count > 0;
    if compound_command_has_cd
        && normalized
            .iter()
            .any(|argv| argv.first().is_some_and(|name| name == "git"))
    {
        return ask_without_suggestions(
            "Compound commands with cd and git require approval to prevent bare repository attacks",
        );
    }

    // Explicit subcommand deny rules must win before path asks.
    for extracted in commands {
        let matching = matching_rules_for_input(
            &extracted.text,
            tool_permission_context,
            BashRuleMatchMode::Prefix,
            false,
        );
        if let Some(rule) = matching.matching_deny_rules.first() {
            return PermissionResult::Deny {
                message: format!("Permission to use Bash with command {command} has been denied."),
                decision_reason: PermissionDecisionReason::Rule { rule: rule.clone() },
                tool_use_id: None,
            };
        }
    }

    let path_result = super::path_validation::check_path_constraints_from_ast(
        commands,
        &cwd.display().to_string(),
        tool_permission_context,
    );
    if matches!(path_result, PermissionResult::Deny { .. }) {
        return path_result;
    }

    let mut decisions = Vec::with_capacity(commands.len());
    for extracted in commands {
        let decision = bash_tool_check_permission(
            &extracted.text,
            tool_permission_context,
            compound_command_has_cd,
            cwd,
        );
        if matches!(decision, PermissionResult::Deny { .. }) {
            return PermissionResult::Deny {
                message: format!("Permission to use Bash with command {command} has been denied."),
                decision_reason: PermissionDecisionReason::SubcommandResults {
                    reasons: BTreeMap::from([(extracted.text.clone(), Box::new(decision))]),
                },
                tool_use_id: None,
            };
        }
        decisions.push((extracted.text.clone(), decision));
    }

    let ask_count = decisions
        .iter()
        .filter(|(_, result)| matches!(result, PermissionResult::Ask { .. }))
        .count();
    let non_allow_count = decisions
        .iter()
        .filter(|(_, result)| !matches!(result, PermissionResult::Allow { .. }))
        .count();
    if matches!(path_result, PermissionResult::Ask { .. }) && ask_count == 0 {
        return path_result;
    }
    if ask_count == 1 && non_allow_count == 1 {
        if let Some((_, result)) = decisions
            .into_iter()
            .find(|(_, result)| matches!(result, PermissionResult::Ask { .. }))
        {
            return result;
        }
        unreachable!("ask_count guarantees an ask result");
    }

    if matches!(exact, PermissionResult::Allow { .. }) {
        return exact;
    }
    if decisions
        .iter()
        .all(|(_, result)| matches!(result, PermissionResult::Allow { .. }))
    {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({ "command": command })),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::SubcommandResults {
                reasons: decisions
                    .into_iter()
                    .map(|(subcommand, result)| (subcommand, Box::new(result)))
                    .collect(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }

    // CC's successful-AST path still sends a single non-allow command through
    // `checkCommandAndSuggestRules` directly; only true multi-command input is
    // wrapped in a `subcommandResults` aggregate for compound-command UI.
    if decisions.len() == 1 {
        return decisions
            .into_iter()
            .next()
            .expect("single decision exists")
            .1;
    }

    let mut suggestions = Vec::new();
    let mut reasons = BTreeMap::new();
    for (subcommand, result) in decisions {
        if !matches!(result, PermissionResult::Allow { .. }) {
            for suggestion in suggestion_for_exact_command(&subcommand) {
                if suggestions.len() >= MAX_SUGGESTED_RULES_FOR_COMPOUND {
                    break;
                }
                if !suggestions.contains(&suggestion) {
                    suggestions.push(suggestion);
                }
            }
        }
        reasons.insert(subcommand, Box::new(result));
    }
    let decision_reason = PermissionDecisionReason::SubcommandResults { reasons };
    PermissionResult::Ask {
        message: create_permission_request_message(Some(&decision_reason)),
        updated_input: None,
        decision_reason: Some(decision_reason),
        suggestions,
        blocked_path: match path_result {
            PermissionResult::Ask { blocked_path, .. } => blocked_path,
            _ => None,
        },
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: build_pending_classifier_check(command, tool_permission_context),
        content_blocks: Vec::new(),
    }
}

fn ask_without_suggestions(reason: &str) -> PermissionResult {
    let decision_reason = PermissionDecisionReason::Other {
        reason: reason.to_string(),
    };
    PermissionResult::Ask {
        message: create_permission_request_message(Some(&decision_reason)),
        updated_input: None,
        decision_reason: Some(decision_reason),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

/// Maps to: CC `bashToolHasPermission(...)` legacy path in external builds.
pub fn bash_tool_has_permission(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    cwd: &std::path::Path,
) -> PermissionResult {
    let command = command.trim();
    let exact_match = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if !matches!(exact_match, PermissionResult::Passthrough { .. }) {
        return exact_match;
    }

    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_COMMAND_INJECTION_CHECK")
            .as_deref(),
    ) {
        let safety_result = super::bash_security::bash_command_is_safe_deprecated(command);
        if let PermissionResult::Ask {
            message,
            decision_reason,
            ..
        } = safety_result
        {
            return PermissionResult::Ask {
                message,
                updated_input: None,
                decision_reason,
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
    }

    let subcommands = split_command_deprecated(command)
        .into_iter()
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();
    if subcommands.len() > MAX_SUBCOMMANDS_FOR_SECURITY_CHECK {
        let reason = PermissionDecisionReason::Other {
            reason: "Command is too complex to analyze safely".to_string(),
        };
        return PermissionResult::Ask {
            message: create_permission_request_message(Some(&reason)),
            updated_input: None,
            decision_reason: Some(reason),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: build_pending_classifier_check(
                command,
                tool_permission_context,
            ),
            content_blocks: Vec::new(),
        };
    }

    if command.contains('|') {
        return super::bash_command_helpers::check_command_operator_permissions(
            command,
            |subcommand| {
                let prefix = get_simple_command_prefix(subcommand);
                check_command_and_suggest_rules(
                    subcommand,
                    tool_permission_context,
                    prefix.as_deref(),
                    command_has_any_cd(command),
                    cwd,
                )
            },
            super::bash_command_helpers::CommandIdentityCheckers::default(),
        );
    }

    if subcommands.len() > 1 {
        let segments = subcommands
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        return super::bash_command_helpers::segmented_command_permission_result(
            command,
            &segments,
            |subcommand| {
                let prefix = get_simple_command_prefix(subcommand);
                check_command_and_suggest_rules(
                    subcommand,
                    tool_permission_context,
                    prefix.as_deref(),
                    command_has_any_cd(command),
                    cwd,
                )
            },
            super::bash_command_helpers::CommandIdentityCheckers::default(),
        );
    }

    let prefix = get_simple_command_prefix(command);
    check_command_and_suggest_rules(
        command,
        tool_permission_context,
        prefix.as_deref(),
        false,
        cwd,
    )
}

/// Maps to CC `peekSpeculativeClassifierCheck(command)`.
pub fn peek_speculative_classifier_check(
    command: &str,
) -> Option<crate::utils::permissions::bash_classifier::ClassifierResult> {
    SPECULATIVE_CLASSIFIER_CHECKS
        .lock()
        .unwrap()
        .get(command)
        .cloned()
}

/// Maps to CC `startSpeculativeClassifierCheck(...)`.
///
/// The external/safe-disabled classifier policy means this normally returns
/// `false` and does not start network/model work. The storage boundary is still
/// present so future classifier enablement can be wired without moving files.
pub fn start_speculative_classifier_check(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    is_non_interactive_session: bool,
) -> bool {
    if !crate::utils::permissions::bash_classifier::is_classifier_permissions_enabled() {
        return false;
    }
    if tool_permission_context.mode == crate::types::permissions::PermissionMode::BypassPermissions
    {
        return false;
    }
    let descriptions =
        crate::utils::permissions::bash_classifier::get_bash_prompt_allow_descriptions(
            tool_permission_context,
        );
    if descriptions.is_empty() {
        return false;
    }
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let result = futures::executor::block_on(
        crate::utils::permissions::bash_classifier::classify_bash_command(
            command,
            &cwd,
            &descriptions,
            crate::utils::permissions::bash_classifier::ClassifierBehavior::Allow,
            is_non_interactive_session,
        ),
    );
    SPECULATIVE_CLASSIFIER_CHECKS
        .lock()
        .unwrap()
        .insert(command.to_string(), result);
    true
}

/// Maps to CC `consumeSpeculativeClassifierCheck(command)`.
pub fn consume_speculative_classifier_check(
    command: &str,
) -> Option<crate::utils::permissions::bash_classifier::ClassifierResult> {
    SPECULATIVE_CLASSIFIER_CHECKS
        .lock()
        .unwrap()
        .remove(command)
}

/// Maps to CC `clearSpeculativeChecks()`.
pub fn clear_speculative_checks() {
    SPECULATIVE_CLASSIFIER_CHECKS.lock().unwrap().clear();
}

/// Maps to CC `awaitClassifierAutoApproval(pendingCheck, ...)`.
pub async fn await_classifier_auto_approval(
    pending_check: crate::utils::permissions::permission_result::PendingClassifierCheck,
    is_non_interactive_session: bool,
) -> Option<PermissionDecisionReason> {
    let classifier_result =
        if let Some(result) = consume_speculative_classifier_check(&pending_check.command) {
            result
        } else {
            crate::utils::permissions::bash_classifier::classify_bash_command(
                &pending_check.command,
                &pending_check.cwd,
                &pending_check.descriptions,
                crate::utils::permissions::bash_classifier::ClassifierBehavior::Allow,
                is_non_interactive_session,
            )
            .await
        };
    classifier_auto_approval_reason(classifier_result)
}

/// Maps to CC `executeAsyncClassifierCheck(...)` as a safe-disabled async seam.
pub async fn execute_async_classifier_check(
    pending_check: crate::utils::permissions::permission_result::PendingClassifierCheck,
    is_non_interactive_session: bool,
) -> Option<PermissionDecisionReason> {
    await_classifier_auto_approval(pending_check, is_non_interactive_session).await
}

fn classifier_auto_approval_reason(
    classifier_result: crate::utils::permissions::bash_classifier::ClassifierResult,
) -> Option<PermissionDecisionReason> {
    if crate::utils::permissions::bash_classifier::is_classifier_permissions_enabled()
        && classifier_result.matches
        && classifier_result.confidence
            == crate::utils::permissions::bash_classifier::ClassifierConfidence::High
    {
        Some(PermissionDecisionReason::Classifier {
            classifier: "bash_allow".to_string(),
            reason: format!(
                "Allowed by prompt rule: \"{}\"",
                classifier_result.matched_description.unwrap_or_default()
            ),
        })
    } else {
        None
    }
}

/// Maps to CC `buildPendingClassifierCheck(...)` with the current safe-disabled
/// classifier policy: real classifier RPCs are not run, but the official
/// metadata shape is preserved when allow descriptions are configured.
pub fn build_pending_classifier_check(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> Option<crate::utils::permissions::permission_result::PendingClassifierCheck> {
    if tool_permission_context.mode == crate::types::permissions::PermissionMode::BypassPermissions
    {
        return None;
    }
    let descriptions =
        crate::utils::permissions::bash_classifier::get_bash_prompt_allow_descriptions(
            tool_permission_context,
        );
    if descriptions.is_empty() {
        return None;
    }
    Some(
        crate::utils::permissions::permission_result::PendingClassifierCheck {
            command: command.to_string(),
            cwd: std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            descriptions,
        },
    )
}

// Tool-local curry only; message formatting belongs to
// `utils/permissions/permissions.ts#createPermissionRequestMessage`.
fn create_permission_request_message(reason: Option<&PermissionDecisionReason>) -> String {
    crate::utils::permissions::permissions::create_permission_request_message(
        super::tool_name::BASH_TOOL_NAME,
        reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionRuleSource, PermissionRuleValue, PermissionUpdateDestination,
    };
    use crate::utils::test_env::PinnedProjectDir;

    #[test]
    fn simple_and_first_word_prefixes_match_official_filters() {
        assert_eq!(
            get_simple_command_prefix("git commit -m fix"),
            Some("git commit".to_string())
        );
        assert_eq!(
            get_simple_command_prefix("NODE_ENV=prod npm run build"),
            Some("npm run".to_string())
        );
        assert_eq!(get_simple_command_prefix("MY_VAR=1 npm run build"), None);
        assert_eq!(get_simple_command_prefix("ls -la"), None);
        assert_eq!(
            get_first_word_prefix("python3 script.py 2>&1 | tail -20"),
            Some("python3".to_string())
        );
        assert_eq!(get_first_word_prefix("bash -lc echo"), None);
    }

    #[test]
    fn suggestion_for_exact_command_prefers_stable_prefixes() {
        let updates = suggestion_for_exact_command("git commit -m fix");
        assert_eq!(
            updates,
            vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "Bash",
                    Some("git commit:*".to_string())
                )],
            }]
        );
        let heredoc = suggestion_for_exact_command("cat <<EOF\nhello\nEOF");
        assert_eq!(
            heredoc,
            vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Bash", Some("cat:*".to_string()))],
            }]
        );
    }

    #[test]
    fn safe_wrapper_and_env_stripping_follow_official_security_boundaries() {
        assert_eq!(
            strip_safe_wrappers("NO_COLOR=1 timeout -k 5 10s git status"),
            "git status"
        );
        assert_eq!(
            strip_safe_wrappers("RUN=/tmp/x git status"),
            "RUN=/tmp/x git status"
        );
        assert_eq!(
            strip_safe_wrappers("# comment\nnohup -- git status"),
            "git status"
        );
        assert_eq!(
            strip_all_leading_env_vars("FOO=a=b denied_command", None),
            "denied_command"
        );
        assert_eq!(
            strip_all_leading_env_vars("PATH=/tmp git status", Some(&BINARY_HIJACK_VARS)),
            "PATH=/tmp git status"
        );
    }

    #[test]
    fn bash_matching_rules_match_official_later_source_metadata_for_duplicate_contents() {
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new(
                super::super::tool_name::BASH_TOOL_NAME,
                Some("git status".to_string()),
            )],
        );
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                super::super::tool_name::BASH_TOOL_NAME,
                Some("git status".to_string()),
            )],
        );

        let matching =
            matching_rules_for_input("git status", &context, BashRuleMatchMode::Exact, false);
        assert_eq!(matching.matching_allow_rules.len(), 1);
        assert_eq!(
            matching.matching_allow_rules[0].source,
            PermissionRuleSource::Session
        );
    }

    #[test]
    fn argv_wrapper_stripping_matches_timeout_and_nice_forms() {
        assert_eq!(
            strip_wrappers_from_argv(&[
                "timeout".into(),
                "-k".into(),
                "5".into(),
                "10s".into(),
                "ls".into()
            ]),
            vec!["ls".to_string()]
        );
        assert_eq!(
            strip_wrappers_from_argv(&["nohup".into(), "--".into(), "rm".into()]),
            vec!["rm".to_string()]
        );
        assert_eq!(
            strip_wrappers_from_argv(&[
                "nice".into(),
                "-n".into(),
                "5".into(),
                "git".into(),
                "status".into()
            ]),
            vec!["git".to_string(), "status".to_string()]
        );
    }

    #[test]
    fn normalized_git_and_cd_detection_handles_wrappers_and_quotes() {
        assert!(is_normalized_git_command("'git' status"));
        assert!(is_normalized_git_command("NO_COLOR=1 xargs git status"));
        assert!(is_normalized_cd_command("FORCE_COLOR=1 cd sub"));
        assert!(is_normalized_cd_command("pushd /tmp"));
        assert!(command_has_any_cd(
            "echo hi && NO_COLOR=1 cd sub && git status"
        ));
        assert!(!is_normalized_git_command("gitlab status"));
    }

    #[test]
    fn shared_rule_helpers_are_reexported_with_bash_names() {
        assert_eq!(permission_rule_extract_prefix("npm:*"), Some("npm"));
        assert!(match_wildcard_pattern("git *", "git status"));
        assert!(matches!(
            bash_permission_rule("git:*"),
            ShellPermissionRule::Prefix { .. }
        ));
        assert_eq!(
            suggestion_for_prefix("npm run"),
            shared_suggestion_for_prefix("Bash", "npm run")
        );
    }

    #[test]
    fn bash_permission_rule_matching_honors_deny_env_stripping_and_allow_wrappers() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("rm:*".to_string()))],
        );
        context.always_allow_rules.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("npm run:*".to_string()),
            )],
        );

        assert!(matches!(
            bash_tool_check_permission("FOO=a=b rm -rf target", &context, false, &std::env::current_dir().unwrap()),
            PermissionResult::Deny { decision_reason: PermissionDecisionReason::Rule { ref rule }, .. }
                if rule.rule_behavior == PermissionBehavior::Deny
        ));
        assert!(matches!(
            bash_tool_check_permission("NODE_ENV=test timeout 10 npm run build", &context, false, &std::env::current_dir().unwrap()),
            PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::Rule { ref rule }), .. }
                if rule.rule_behavior == PermissionBehavior::Allow
        ));
    }

    #[test]
    fn sandbox_auto_allow_cannot_bypass_rules_through_pipes_or_background_ops() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("git push:*".to_string()),
            )],
        );
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("rm:*".to_string()))],
        );
        assert!(matches!(
            check_sandbox_auto_allow("printf x | git push origin main", &context),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            check_sandbox_auto_allow("printf x & rm -f file", &context),
            PermissionResult::Ask { .. }
        ));
        assert_eq!(split_command_deprecated("printf 'x|y' && echo ok").len(), 2);
        assert!(has_obscured_command_names("g\"\"it push origin main"));
        assert!(has_obscured_command_names("echo $(git push origin main)"));

        let crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } =
            crate::utils::bash::ast::parse_for_security("if true; then git push origin main; fi")
        else {
            panic!("control-flow sandbox fixture parses");
        };
        assert!(matches!(
            check_sandbox_auto_allow_from_ast(
                "if true; then git push origin main; fi",
                &commands,
                &context,
            ),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn permission_path_resolution_uses_tool_context_cwd() {
        let root = std::env::temp_dir().join(format!(
            "cometix-bash-permission-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let child = root.join("child");
        let allowed = root.join("allowed");
        let unrelated = std::env::temp_dir()
            .join(format!(
                "cometix-bash-unrelated-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .join("deeper");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(allowed.join("file"), "ok").unwrap();
        let mut context = ToolPermissionContext::default();
        context.additional_working_directories.insert(
            root.display().to_string(),
            crate::types::permissions::AdditionalWorkingDirectory {
                path: root.display().to_string(),
                source: crate::types::permissions::PermissionRuleSource::Session,
            },
        );

        assert!(matches!(
            bash_tool_has_permission("cat ../allowed/file", &context, &child),
            PermissionResult::Allow { .. }
        ));
        assert!(!matches!(
            bash_tool_has_permission("cat ../allowed/file", &context, &unrelated),
            PermissionResult::Allow { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(unrelated.parent().unwrap());
    }

    #[test]
    fn bash_tool_check_permission_runs_path_sed_mode_and_read_only_layers() {
        // The path layer resolves against the project dir.
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let default_context = ToolPermissionContext::default();
        assert!(matches!(
            bash_tool_check_permission("git status", &default_context, false, &std::env::current_dir().unwrap()),
            PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::Other { ref reason }), .. }
                if reason == "Read-only command is allowed"
        ));
        assert!(matches!(
            bash_tool_check_permission("sed 's/a/b/w /tmp/out'", &default_context, false, &std::env::current_dir().unwrap()),
            PermissionResult::Ask { ref message, .. }
                if message == "sed command requires approval (contains potentially dangerous operations)"
        ));
        let accept = ToolPermissionContext {
            mode: crate::types::permissions::PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            bash_tool_check_permission(
                "mkdir target",
                &accept,
                false,
                &std::env::current_dir().unwrap()
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::Mode { .. }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn speculative_classifier_seams_are_safe_disabled_like_external_build() {
        clear_speculative_checks();
        let context = ToolPermissionContext::default();
        assert!(!start_speculative_classifier_check(
            "cargo test",
            &context,
            false
        ));
        assert!(peek_speculative_classifier_check("cargo test").is_none());
        assert!(consume_speculative_classifier_check("cargo test").is_none());

        let pending = crate::utils::permissions::permission_result::PendingClassifierCheck {
            command: "cargo test".to_string(),
            cwd: ".".to_string(),
            descriptions: vec!["run tests".to_string()],
        };
        assert!(
            await_classifier_auto_approval(pending.clone(), false)
                .await
                .is_none()
        );
        assert!(
            execute_async_classifier_check(pending, false)
                .await
                .is_none()
        );
    }

    #[test]
    fn ast_permission_path_checks_extracted_control_flow_without_legacy_resplit() {
        fn parsed(source: &str) -> Vec<crate::utils::bash::ast::SimpleCommand> {
            match crate::utils::bash::ast::parse_for_security(source) {
                crate::utils::bash::ast::ParseForSecurityResult::Simple { commands } => commands,
                other => panic!("expected simple AST for {source:?}, got {other:?}"),
            }
        }

        let context = ToolPermissionContext::default();
        let cwd = std::env::current_dir().unwrap();
        let source = "if true; then echo hi; fi";
        assert!(matches!(
            bash_tool_has_permission_from_ast(source, &parsed(source), &context, &cwd),
            PermissionResult::Allow { .. }
        ));

        let source = "P=/etc/passwd && cat \"$P\"";
        assert!(matches!(
            bash_tool_has_permission_from_ast(source, &parsed(source), &context, &cwd),
            PermissionResult::Ask {
                blocked_path: Some(_),
                ..
            }
        ));

        let mut denied = ToolPermissionContext::default();
        denied.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("echo:*".to_string()))],
        );
        assert!(matches!(
            bash_tool_has_permission_from_ast(
                "if true; then echo hi; fi",
                &parsed("if true; then echo hi; fi"),
                &denied,
                &cwd,
            ),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn bash_tool_has_permission_shapes_compound_and_security_results() {
        let context = ToolPermissionContext::default();
        assert!(matches!(
            bash_tool_has_permission(
                "echo hi && cargo test",
                &context,
                &std::env::current_dir().unwrap()
            ),
            PermissionResult::Ask {
                decision_reason: Some(PermissionDecisionReason::SubcommandResults { .. }),
                ..
            }
        ));
        assert!(matches!(
            bash_tool_has_permission("echo $(id)", &context, &std::env::current_dir().unwrap()),
            PermissionResult::Ask { ref message, ref suggestions, .. }
                if message == "Command contains $() command substitution" && suggestions.is_empty()
        ));
    }
}
