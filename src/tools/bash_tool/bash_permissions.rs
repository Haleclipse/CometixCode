//! Maps to: CC `tools/BashTool/bashPermissions.ts`.
//!
//! This module owns Bash permission rule normalization, command permission
//! checks, and the safe-disabled classifier seams used by `useCanUseTool`.
//! All permission paths use fail-closed tree-sitter Bash security analysis.

use regex::Regex;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::path_validation::strip_wrappers_from_argv;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{PermissionRule, PermissionUpdate};
use crate::utils::bash::ast::{self, ParseForSecurityResult, SemanticCheckResult, SimpleCommand};
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
pub const MAX_SUBCOMMANDS_FOR_SECURITY_CHECK: usize = ast::MAX_COMMANDS_FOR_SECURITY_CHECK;
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
    if command.contains('\n')
        && let Some(first_line) = command
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return shared_suggestion_for_prefix(super::tool_name::BASH_TOOL_NAME, first_line);
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
    if let Ok(tokens) = crate::utils::bash::shell_quote::try_parse_shell_command(&stripped)
        && let Some(crate::utils::bash::shell_quote::ParseEntry::String(first)) = tokens.first()
    {
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
    stripped == "git" || stripped.starts_with("git ")
}

/// Maps to: CC `isNormalizedCdCommand(command)`.
pub fn is_normalized_cd_command(command: &str) -> bool {
    let stripped = strip_safe_wrappers(command);
    if let Ok(tokens) = crate::utils::bash::shell_quote::try_parse_shell_command(&stripped)
        && let Some(crate::utils::bash::shell_quote::ParseEntry::String(first)) = tokens.first()
    {
        return matches!(first.as_str(), "cd" | "pushd" | "popd");
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

fn is_safe_env_var(var_name: &str) -> bool {
    SAFE_ENV_VARS.contains(&var_name)
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
                    || candidate == format!("xargs {prefix}")
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

/// Checks a verified simple command; argv and redirects are validated once
/// across the whole AST before command allow rules or permission modes apply.
fn bash_tool_check_permission(
    extracted: &SimpleCommand,
    tool_permission_context: &ToolPermissionContext,
    cwd: &std::path::Path,
) -> PermissionResult {
    let command = extracted.text.trim();
    let exact_match = bash_tool_check_exact_match_permission(command, tool_permission_context);
    if matches!(
        exact_match,
        PermissionResult::Deny { .. } | PermissionResult::Ask { .. }
    ) {
        return exact_match;
    }

    let matching = matching_rules_for_ast_command(extracted, tool_permission_context);
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

    let writes_file = extracted.redirects.iter().any(|redirect| {
        redirect.target != "/dev/null"
            && match redirect.op.as_str() {
                ">" | ">>" | ">|" | "&>" | "&>>" => true,
                ">&" => {
                    redirect.target != "-" && !redirect.target.chars().all(|ch| ch.is_ascii_digit())
                }
                _ => false,
            }
    });
    if !writes_file && super::read_only_validation::check_read_only_constraints(command, cwd) {
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

/// Deny rules are checked before syntax/semantic asks, including normalized
/// argv so quoting a command name cannot conceal a denied command.
fn parse_permission_commands(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> Result<Vec<SimpleCommand>, Box<PermissionResult>> {
    let parsed = ast::parse_for_security(command);
    let full = matching_rules_for_input(
        command,
        tool_permission_context,
        BashRuleMatchMode::Prefix,
        false,
    );
    let mut deny = full.matching_deny_rules.first().cloned();
    if let ParseForSecurityResult::Simple { commands } = &parsed {
        deny = deny.or_else(|| {
            commands.iter().find_map(|extracted| {
                matching_rules_for_ast_command(extracted, tool_permission_context)
                    .matching_deny_rules
                    .into_iter()
                    .next()
            })
        });
    }
    if let Some(rule) = deny {
        return Err(Box::new(PermissionResult::Deny {
            message: format!("Permission to use Bash with command {command} has been denied."),
            decision_reason: PermissionDecisionReason::Rule { rule },
            tool_use_id: None,
        }));
    }
    let commands = match parsed {
        ParseForSecurityResult::Simple { commands } => commands,
        ParseForSecurityResult::TooComplex { reason, .. } => {
            return Err(Box::new(ask_without_suggestions(&reason)));
        }
        ParseForSecurityResult::ParseUnavailable => {
            return Err(Box::new(ask_without_suggestions(
                "Bash syntax analysis is unavailable; manual approval is required",
            )));
        }
    };
    if commands.len() > MAX_SUBCOMMANDS_FOR_SECURITY_CHECK {
        return Err(Box::new(ask_without_suggestions(
            "Command is too complex to analyze safely",
        )));
    }
    if let SemanticCheckResult::Unsafe { reason } = ast::check_semantics(&commands) {
        return Err(Box::new(ask_without_suggestions(&reason)));
    }
    Ok(commands)
}

fn matching_rules_for_ast_command(
    command: &SimpleCommand,
    context: &ToolPermissionContext,
) -> MatchingBashRules {
    let mut matching =
        matching_rules_for_input(&command.text, context, BashRuleMatchMode::Prefix, true);
    let normalized = matching_rules_for_input(
        &strip_wrappers_from_argv(&command.argv).join(" "),
        context,
        BashRuleMatchMode::Prefix,
        true,
    );
    matching
        .matching_deny_rules
        .extend(normalized.matching_deny_rules);
    matching
        .matching_ask_rules
        .extend(normalized.matching_ask_rules);
    matching
}

/// Maps to CC `checkSandboxAutoAllow(...)`: explicit deny wins across the
/// full command and every compound segment, ask is next, otherwise OS-sandbox
/// execution is auto-allowed.
pub fn check_sandbox_auto_allow(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    let commands = match parse_permission_commands(command, tool_permission_context) {
        Ok(commands) => commands,
        Err(result) => return *result,
    };
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
    for extracted in &commands {
        let matching = matching_rules_for_ast_command(extracted, tool_permission_context);
        first_ask = first_ask.or_else(|| matching.matching_ask_rules.first().cloned());
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

/// Maps to CC `bashToolHasPermission(...)` using the native AST projection.
pub fn bash_tool_has_permission(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
    cwd: &std::path::Path,
) -> PermissionResult {
    use std::collections::BTreeMap;

    let commands = match parse_permission_commands(command, tool_permission_context) {
        Ok(commands) => commands,
        Err(result) => return *result,
    };
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

    if compound_command_has_cd && commands.len() > 1 {
        return ask_without_suggestions(
            "Compound commands that change directories require approval to resolve paths safely",
        );
    }

    let mut path_result = super::path_validation::check_path_constraints(
        &commands,
        &cwd.display().to_string(),
        tool_permission_context,
    );
    if let PermissionResult::Ask {
        decision_reason,
        message,
        ..
    } = &mut path_result
        && !matches!(decision_reason, Some(PermissionDecisionReason::Rule { .. }))
    {
        *decision_reason = Some(PermissionDecisionReason::SafetyCheck {
            reason: message.clone(),
            classifier_approvable: false,
        });
    }
    if !matches!(path_result, PermissionResult::Passthrough { .. }) {
        return path_result;
    }

    let mut decisions = Vec::with_capacity(commands.len());
    for extracted in &commands {
        let decision = bash_tool_check_permission(extracted, tool_permission_context, cwd);
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
    if ask_count > 0 {
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
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: build_pending_classifier_check(command, tool_permission_context),
        content_blocks: Vec::new(),
    }
}

fn ask_without_suggestions(reason: &str) -> PermissionResult {
    let decision_reason = PermissionDecisionReason::SafetyCheck {
        reason: reason.to_string(),
        classifier_approvable: false,
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
            bash_tool_has_permission("FOO=a=b rm -rf target", &context, &std::env::current_dir().unwrap()),
            PermissionResult::Deny { decision_reason: PermissionDecisionReason::Rule { ref rule }, .. }
                if rule.rule_behavior == PermissionBehavior::Deny
        ));
        assert!(matches!(
            bash_tool_has_permission("NODE_ENV=test timeout 10 npm run build", &context, &std::env::current_dir().unwrap()),
            PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::SubcommandResults { ref reasons }), .. }
                if matches!(reasons.get("NODE_ENV=test timeout 10 npm run build").map(Box::as_ref), Some(PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::Rule { rule }), .. }) if rule.rule_behavior == PermissionBehavior::Allow)
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
        for source in [
            "g\"\"it push origin main",
            "echo \"status: $(git push origin main)\"",
            "if true; then git push origin main; fi",
            "(git push origin main)",
        ] {
            assert!(
                matches!(
                    check_sandbox_auto_allow(source, &context),
                    PermissionResult::Deny { .. }
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn permission_path_resolution_uses_tool_context_cwd() {
        let root = std::env::temp_dir().join(format!(
            "nyan-bash-permission-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let child = root.join("child");
        let allowed = root.join("allowed");
        let unrelated = std::env::temp_dir()
            .join(format!(
                "nyan-bash-unrelated-{}",
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
            bash_tool_has_permission("git status", &default_context, &std::env::current_dir().unwrap()),
            PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::SubcommandResults { ref reasons }), .. }
                if matches!(reasons.get("git status").map(Box::as_ref), Some(PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::Other { reason }), .. }) if reason == "Read-only command is allowed")
        ));
        assert!(matches!(
            bash_tool_has_permission("sed 's/a/b/w /tmp/out'", &default_context, &std::env::current_dir().unwrap()),
            PermissionResult::Ask { ref message, .. }
                if message == "sed command requires approval (contains potentially dangerous operations)"
        ));
        let accept = ToolPermissionContext {
            mode: crate::types::permissions::PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            bash_tool_has_permission(
                "mkdir target",
                &accept,
                &std::env::current_dir().unwrap()
            ),
            PermissionResult::Allow {
                decision_reason: Some(PermissionDecisionReason::SubcommandResults { ref reasons }),
                ..
            } if matches!(reasons.get("mkdir target").map(Box::as_ref), Some(PermissionResult::Allow { decision_reason: Some(PermissionDecisionReason::Mode { .. }), .. }))
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
        let context = ToolPermissionContext::default();
        let cwd = std::env::current_dir().unwrap();
        let source = "if true; then echo hi; fi";
        assert!(matches!(
            bash_tool_has_permission(source, &context, &cwd),
            PermissionResult::Allow { .. }
        ));

        let source = "P=/etc/passwd && cat \"$P\"";
        assert!(matches!(
            bash_tool_has_permission(source, &context, &cwd),
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
            bash_tool_has_permission("if true; then echo hi; fi", &denied, &cwd),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn ast_permissions_allow_verified_static_shell_structure() {
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let context = ToolPermissionContext::default();
        let cwd = std::env::current_dir().unwrap();
        for source in [
            "echo hello | head -n 1",
            "(echo hello && pwd)",
            "if true; then echo hello; else echo goodbye; fi",
            "while false; do echo hello; done",
            "for item in one two; do echo hello; done",
            "for i in 1 2 3; do echo progress-$i; sleep 1.2; done",
            "for file in Cargo.toml Cargo.lock; do cat \"$file\"; done",
            "for cmd in echo pwd; do $cmd; done",
            "! false",
            "cat <<'EOF'\n$(not_a_command) | literal data\nEOF",
            "echo hello > /dev/null",
        ] {
            let result = bash_tool_has_permission(source, &context, &cwd);
            assert!(
                matches!(result, PermissionResult::Allow { .. }),
                "{source:?}: {result:?}"
            );
        }
    }

    #[test]
    fn ast_security_asks_cannot_be_overridden_by_allow_rules_or_sandbox() {
        let cwd = std::env::current_dir().unwrap();
        let mut sources = vec![
            "echo 'unterminated".to_string(),
            "echo $(id)".to_string(),
            "echo \"prefix: $(id)\"".to_string(),
            "cat \"prefix$HOME\"".to_string(),
            "cat <(echo hello)".to_string(),
            "cat <<EOF\n$(id)\nEOF".to_string(),
            "eval 'echo hello'".to_string(),
            "PATH=/untrusted; echo hello".to_string(),
            "LD_PRELOAD=library.so echo hello".to_string(),
            "env PATH=/untrusted echo hello".to_string(),
            "FOO=$(cat Cargo.toml) echo hello".to_string(),
            "FOO=$(cat Cargo.toml)".to_string(),
            "export PATH=/untrusted".to_string(),
            "bash <<'EOF'\necho hello\nEOF".to_string(),
            "/bin/bash -c 'echo hello'".to_string(),
            "f() { echo hello; }; f".to_string(),
            "P=/etc/passwd; false && P=inside; cat \"$P\"".to_string(),
            "P=inside; if true; then P=/etc/passwd; fi; cat \"$P\"".to_string(),
            "P=inside; while true; do cat \"$P\"; P=/etc/passwd; done".to_string(),
            "P=inside; read P; cat \"$P\"".to_string(),
            "P=inside; printf -v P /etc/passwd; cat \"$P\"".to_string(),
            "P=inside; true && P=/etc/passwd && false || cat \"$P\"".to_string(),
            "P=inside; while cat \"$P\" && P=/etc/passwd; do echo hello; done".to_string(),
            "REPLY=inside; read; cat \"$REPLY\"".to_string(),
            "P=inside; time read P; cat \"$P\"".to_string(),
            "P=inside; time export P=/etc/passwd; cat \"$P\"".to_string(),
            "P=inside; e\"\"xport P=/etc/passwd; cat \"$P\"".to_string(),
            "P=inside; export 'P=/etc/passwd'; cat \"$P\"".to_string(),
            "P=inside; export \"P=/etc/passwd\"; cat \"$P\"".to_string(),
            "P=inside; wait -np P; cat \"$P\"".to_string(),
            "BASH_REMATCH=inside; cat \"$BASH_REMATCH\"".to_string(),
            "P=/etc/passwd; readonly P; P=inside; cat \"$P\"".to_string(),
            "P=/etc/passwd; declare -r P; P=inside; cat \"$P\"".to_string(),
            "P+=inside; cat \"$P\"".to_string(),
            "RANDOM=1; cat \"$RANDOM\"".to_string(),
            "P=inside; echo \"$P\" $((1+2))".to_string(),
            "echo \"number: $((1+2))\"".to_string(),
            "[[ 'P=1' -eq 1 ]]".to_string(),
            "unset PATH; echo hello".to_string(),
            "read PATH; echo hello".to_string(),
            "set -f; echo hello".to_string(),
            "shopt -s lastpipe; echo hello".to_string(),
            "for PATH in /untrusted; do echo hello; done".to_string(),
            "for IFS in /; do echo hello; done".to_string(),
            "for RANDOM in 1; do cat \"$RANDOM\"; done".to_string(),
            "for file in ../*; do cat \"$file\"; done".to_string(),
            "for file in \"$(pwd)\"; do echo hello; done".to_string(),
            "for file in \"$HOME\"; do echo hello; done".to_string(),
            "for file; do echo hello; done".to_string(),
            "for file in; do echo hello; done".to_string(),
            "for i in 1 2; do i=3; echo $i; done".to_string(),
            "p=inside; for i in 1 2; do cat \"$p\"; for p in /etc/passwd; do echo hello; done; done".to_string(),
            "for file in Cargo.toml; do echo hello; done; cat \"$file\"".to_string(),
            "cat \"$(cat 3<<'EOF'\nCargo.toml\nEOF\n)\"".to_string(),
            "cat \"$(cat <<-'EOF'\n\tCargo.toml\nEOF\n)\"".to_string(),
        ];
        sources.push(format!("echo {}", "x".repeat(10_001)));
        sources.push(format!("P=x; {} cat \"$P\"", "P=\"$P$P\";".repeat(20)));
        sources.push(format!("{}echo hello{}", "(".repeat(200), ")".repeat(200)));
        sources.push(
            std::iter::repeat_n("echo hello", MAX_SUBCOMMANDS_FOR_SECURITY_CHECK + 1)
                .collect::<Vec<_>>()
                .join("; "),
        );
        for source in sources {
            for rule in [Some(source.clone()), None] {
                let mut context = ToolPermissionContext::default();
                context.always_allow_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Bash", rule)],
                );
                for result in [
                    bash_tool_has_permission(&source, &context, &cwd),
                    check_sandbox_auto_allow(&source, &context),
                ] {
                    assert!(
                        matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                        "security ask required for {source:?}: {result:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ast_for_environment_rebinding_and_select_always_require_safety_approval() {
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        // Every command in the N1/N2 review table, including its sandbox-only
        // REPLY case, must fail before any auto-allow policy is considered.
        for source in [
            "for HOME in /etc; do cat ~/passwd; done",
            "for HOME in /etc; do cat < ~/passwd; done",
            "for HOME in evilhome; do git status; done",
            "for GIT_DIR in /tmp/x; do git status; done",
            "for XDG_CONFIG_HOME in evilhome; do git status; done",
            "for GIT_CONFIG_GLOBAL in evilhome/config; do git status; done",
            "for LESSOPEN in x; do git log; done",
            "for HOME in /etc; do cd; done",
            "for OLDPWD in /etc; do cd -; done",
            "for HOME in /tmp; do echo x > ~/f; done",
            "echo 9 | select p in src; do cat \"$p/etc/hostname\"; done",
            "select p in Cargo.toml; do cat \"$p\"; done",
            "REPLY=status; echo push | select x in a; do git \"$REPLY\" origin main; break; done",
            "for path in /tmp; do cat ~/passwd; done",
            "for NYAN_UNKNOWN_ENV in x; do git status; done",
            "export file; for file in inside; do cat \"$file\"; done",
            "declare -x file; for file in inside; do cat \"$file\"; done",
            "arg=file; export \"$arg\"; for file in inside; do cat \"$file\"; done",
            "for file in '~'; do cat $file; done",
            "for file in '<1-9>'; do cat $file; done",
        ] {
            for mode in [
                crate::types::permissions::PermissionMode::Default,
                crate::types::permissions::PermissionMode::AcceptEdits,
            ] {
                for allow in [
                    None,
                    Some(Some(source.to_string())),
                    Some(None),
                    Some(Some("echo:*".to_string())),
                ] {
                    for deny in [false, true] {
                        let mut context = ToolPermissionContext {
                            mode,
                            ..ToolPermissionContext::default()
                        };
                        if let Some(rule) = &allow {
                            context.always_allow_rules.insert(
                                PermissionRuleSource::Session,
                                vec![PermissionRuleValue::new("Bash", rule.clone())],
                            );
                        }
                        let rules = if deny {
                            &mut context.always_deny_rules
                        } else {
                            &mut context.always_ask_rules
                        };
                        rules.insert(
                            PermissionRuleSource::Session,
                            vec![PermissionRuleValue::new(
                                "Bash",
                                Some("git push:*".to_string()),
                            )],
                        );
                        for result in [
                            bash_tool_has_permission(source, &context, &cwd),
                            check_sandbox_auto_allow(source, &context),
                        ] {
                            assert!(
                                matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                                "safety ask required for {source:?}: {result:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ast_declaration_substitution_loops_cannot_override_safety() {
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let started = std::time::Instant::now();
        for builtin in ["export", "declare", "typeset"] {
            for operand in [
                "\"$(for i in x; do echo hello; done)\"",
                "\"other=$(for i in x; do echo hello; done)\"",
                "other\"$(for i in x; do echo hello; done)\"",
                "\"`for i in x; do echo hello; done`\"",
                "\"$(echo \"$(for i in x; do echo hello; done)\")\"",
                "\"$(for i in x; do for j in y; do echo hello; done; done)\"",
                "\"$(for i in x; do export other; echo hello; done)\"",
                "\"$(for i in x; do declare other; echo hello; done)\"",
                "\"$(for i in x; do typeset other; echo hello; done)\"",
            ] {
                let source = format!("{builtin} {operand}");
                for mode in [
                    crate::types::permissions::PermissionMode::Default,
                    crate::types::permissions::PermissionMode::AcceptEdits,
                ] {
                    for allow in [Some(source.clone()), None] {
                        let mut context = ToolPermissionContext {
                            mode,
                            ..ToolPermissionContext::default()
                        };
                        context.always_allow_rules.insert(
                            PermissionRuleSource::Session,
                            vec![PermissionRuleValue::new("Bash", allow)],
                        );
                        for result in [
                            bash_tool_has_permission(&source, &context, &cwd),
                            check_sandbox_auto_allow(&source, &context),
                        ] {
                            assert!(
                                matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                                "declaration loop must safely ask: {source:?}: {result:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn ast_inherited_allowlisted_loop_variables_cannot_override_safety() {
        use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let cwd = std::env::current_dir().unwrap();
        for name in ["i", "item", "file", "cmd"] {
            for value in ["", "inherited"] {
                let _inherited = EnvVarGuard::set(name, value);
                let source = format!("for {name} in Cargo.toml; do cat \"${name}\"; done");
                for allow in [Some(source.clone()), None] {
                    let mut context = ToolPermissionContext::default();
                    context.always_allow_rules.insert(
                        PermissionRuleSource::Session,
                        vec![PermissionRuleValue::new("Bash", allow)],
                    );
                    for result in [
                        bash_tool_has_permission(&source, &context, &cwd),
                        check_sandbox_auto_allow(&source, &context),
                    ] {
                        assert!(
                            matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                            "inherited variable must safely ask: {source:?}: {result:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ast_session_exports_and_hook_scripts_prevent_literal_loop_auto_allow() {
        use crate::utils::session_env_vars::{
            delete_session_env_var, get_session_env_vars, set_session_env_var,
        };
        use crate::utils::session_environment::invalidate_session_env_cache;
        use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

        struct Restore {
            variable: Option<String>,
            script_path: std::path::PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(value) = self.variable.take() {
                    set_session_env_var("file", value);
                } else {
                    delete_session_env_var("file");
                }
                invalidate_session_env_cache();
                let _ = std::fs::remove_file(&self.script_path);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = Restore {
            variable: get_session_env_vars().get("file").cloned(),
            script_path: std::env::temp_dir().join(format!(
                "nyan-ast-loop-env-{}.sh",
                uuid::Uuid::new_v4().simple()
            )),
        };
        let _file = EnvVarGuard::unset("file");
        let _env_file = EnvVarGuard::unset("NYAN_ENV_FILE");
        let _writes = EnvVarGuard::set("NYAN_WRITE_ENABLED", "0");
        invalidate_session_env_cache();
        let cwd = std::env::current_dir().unwrap();
        let source = "for file in Cargo.toml; do cat \"$file\"; done";
        let assert_safety_ask = || {
            for allow in [Some(source.to_string()), None] {
                let mut context = ToolPermissionContext::default();
                context.always_allow_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Bash", allow)],
                );
                for result in [
                    bash_tool_has_permission(source, &context, &cwd),
                    check_sandbox_auto_allow(source, &context),
                ] {
                    assert!(
                        matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                        "session state must safely ask: {result:?}"
                    );
                }
            }
        };
        set_session_env_var("file", "");
        assert_safety_ask();
        delete_session_env_var("file");
        std::fs::write(&_restore.script_path, "export file=before").unwrap();
        let _script = EnvVarGuard::set("NYAN_ENV_FILE", &_restore.script_path);
        invalidate_session_env_cache();
        assert_safety_ask();
    }

    #[test]
    fn ast_declaration_semantics_cannot_be_overridden_by_any_allow() {
        let cwd = std::env::current_dir().unwrap();
        for builtin in ["declare", "typeset"] {
            for option in [
                "-l", "-u", "-p", "-f", "-F", "-xl", "-ux", "-xp", "-pf", "+l",
            ] {
                for source in [
                    format!(
                        "{builtin} {option} action; for action in PUSH; do git \"$action\" origin main; done"
                    ),
                    format!("P=/outside/file; {builtin} {option} P=inside; cat \"$P\""),
                    format!("P=/outside/file; {builtin} {option} P=inside; echo x > \"$P\""),
                ] {
                    for allow in [Some(source.clone()), None] {
                        for deny in [false, true] {
                            let mut context = ToolPermissionContext::default();
                            context.always_allow_rules.insert(
                                PermissionRuleSource::Session,
                                vec![PermissionRuleValue::new("Bash", allow.clone())],
                            );
                            let rules = if deny {
                                &mut context.always_deny_rules
                            } else {
                                &mut context.always_ask_rules
                            };
                            rules.insert(
                                PermissionRuleSource::Session,
                                vec![PermissionRuleValue::new(
                                    "Bash",
                                    Some("git push:*".to_string()),
                                )],
                            );
                            for result in [
                                bash_tool_has_permission(&source, &context, &cwd),
                                check_sandbox_auto_allow(&source, &context),
                            ] {
                                assert!(
                                    matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                                    "unmodeled declarations must safely ask: {source:?}: {result:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn ast_newline_paths_preserve_symlink_and_read_deny_boundaries() {
        let root = std::env::temp_dir().join(format!(
            "nyan-ast-newline-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cwd = root.join("project");
        let outside = root.join("outside");
        std::fs::create_dir_all(cwd.join("safe")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(cwd.join("safe/file"), "inside fixture").unwrap();
        std::fs::write(outside.join("file"), "outside fixture").unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join("safe\n")).unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join("\n")).unwrap();
        let _project_dir = PinnedProjectDir::at(&cwd);
        for source in [
            "for dir in \"safe\n\"; do cat \"$dir/file\"; done",
            "for dir in \"\n\"; do cat \"$dir/file\"; done",
            "cat \"safe\n/file\"",
            "cat < \"safe\n/file\"",
            "echo x > \"safe\n/file\"",
            "for dir in \"safe\n\"; do echo x >> \"$dir/file\"; done",
        ] {
            for allow in [Some(source.to_string()), None] {
                let mut context = ToolPermissionContext::default();
                context.always_allow_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Bash", allow)],
                );
                let result = bash_tool_has_permission(source, &context, &cwd);
                assert!(
                    matches!(
                        result,
                        PermissionResult::Ask {
                            blocked_path: Some(_),
                            ..
                        }
                    ),
                    "{source:?}: {result:?}"
                );
            }
        }
        std::fs::create_dir_all(cwd.join("denied\n")).unwrap();
        std::fs::write(cwd.join("denied\n/file"), "denied fixture").unwrap();
        for source in [
            "cat \"denied\n/file\"",
            "cat < \"denied\n/file\"",
            "for dir in \"denied\n\"; do cat \"$dir/file\"; done",
        ] {
            for allow in [Some(source.to_string()), None] {
                let mut context = ToolPermissionContext::default();
                context.always_allow_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Bash", allow)],
                );
                context.always_deny_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new(
                        "Read",
                        Some(format!("/{}", cwd.join("denied\n/file").display())),
                    )],
                );
                let result = bash_tool_has_permission(source, &context, &cwd);
                assert!(
                    matches!(result, PermissionResult::Deny { .. }),
                    "{source:?}: {result:?}"
                );
            }
        }
        let result = bash_tool_has_permission(
            "echo x > \"output\n\"",
            &ToolPermissionContext {
                mode: crate::types::permissions::PermissionMode::AcceptEdits,
                ..ToolPermissionContext::default()
            },
            &cwd,
        );
        assert!(
            matches!(result, PermissionResult::Passthrough { .. }),
            "{result:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ast_projection_work_budget_cannot_be_overridden_by_allow_or_sandbox() {
        let cwd = std::env::current_dir().unwrap();
        for source in [
            "for i in x; do # empty\ndone",
            "for i in 1 2 3 4 5 6 7 8; do for j in 1 2 3 4 5 6 7 8; do :; done; done",
            "echo \"prefix: $(for i in 1 2 3 4 5 6 7 8; do for j in 1 2 3 4 5 6 7 8; do :; done; done)\"",
        ] {
            for allow in [Some(source.to_string()), None] {
                let mut context = ToolPermissionContext::default();
                context.always_allow_rules.insert(
                    PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Bash", allow)],
                );
                for result in [
                    bash_tool_has_permission(source, &context, &cwd),
                    check_sandbox_auto_allow(source, &context),
                ] {
                    assert!(
                        matches!(result, PermissionResult::Ask { ref suggestions, pending_classifier_check: None, .. } if suggestions.is_empty()),
                        "{source:?}: {result:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ast_rules_cover_every_extracted_command_before_allowing() {
        let cwd = std::env::current_dir().unwrap();
        for source in [
            "printf x | git push origin main",
            "(git push origin main)",
            "if true; then git push origin main; fi",
            "for action in status push; do git $action origin main; done",
            "action=push; declare action=status sub=\"$action\"; git \"$sub\" origin main",
            "action=push; typeset action=status sub=\"$action\"; git \"$sub\" origin main",
            "action=push; export action=status sub=\"$action\"; git \"$sub\" origin main",
            "while true; git push origin main; do echo hello; done",
            "until false; git push origin main; do echo hello; done",
            "echo \"prefix: $(git push origin main)\"",
            "g\"\"it push origin main",
            "env FOO=x timeout 5 git push origin main",
            "cat <<'EOF'\nliteral data\nEOF\ngit push origin main",
        ] {
            let mut context = ToolPermissionContext::default();
            context.always_allow_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Bash", Some(source.to_string()))],
            );
            context.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new(
                    "Bash",
                    Some("git push:*".to_string()),
                )],
            );
            assert!(
                matches!(
                    bash_tool_has_permission(source, &context, &cwd),
                    PermissionResult::Ask { .. }
                ),
                "{source}"
            );
            assert!(
                matches!(
                    check_sandbox_auto_allow(source, &context),
                    PermissionResult::Ask { .. }
                ),
                "{source}"
            );
            context.always_deny_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new(
                    "Bash",
                    Some("git push:*".to_string()),
                )],
            );
            assert!(
                matches!(
                    bash_tool_has_permission(source, &context, &cwd),
                    PermissionResult::Deny { .. }
                ),
                "{source}"
            );
            assert!(
                matches!(
                    check_sandbox_auto_allow(source, &context),
                    PermissionResult::Deny { .. }
                ),
                "{source}"
            );
        }
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("git push:*".to_string()),
            )],
        );
        let literal_heredoc = "cat <<'EOF'\ngit push origin main\nEOF";
        assert!(matches!(
            bash_tool_has_permission(literal_heredoc, &context, &cwd),
            PermissionResult::Allow { .. }
        ));
        assert!(matches!(
            check_sandbox_auto_allow(literal_heredoc, &context),
            PermissionResult::Allow { .. }
        ));
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("eval:*".to_string()))],
        );
        assert!(matches!(
            bash_tool_has_permission("echo hi | eval 'echo bad'", &context, &cwd),
            PermissionResult::Deny { .. }
        ));
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("cat:*".to_string()))],
        );
        let source = "echo \"$(cat <<'EOF'\nhello\nEOF\n)\"";
        assert!(matches!(
            bash_tool_has_permission(source, &context, &cwd),
            PermissionResult::Deny { .. }
        ));
        assert!(matches!(
            check_sandbox_auto_allow(source, &context),
            PermissionResult::Deny { .. }
        ));
    }

    #[test]
    fn ast_projection_preserves_environment_and_redirection_permission_boundaries() {
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let mut context = ToolPermissionContext {
            mode: crate::types::permissions::PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        let target = cwd.join(format!("ast-output-{}", uuid::Uuid::new_v4().simple()));
        for source in [
            format!("echo hello > {}", target.display()),
            format!("echo hello >> {}", target.display()),
            format!("echo hello &> {}", target.display()),
        ] {
            let result = bash_tool_has_permission(&source, &context, &cwd);
            assert!(
                matches!(result, PermissionResult::Passthrough { .. }),
                "{source:?}: {result:?}"
            );
        }
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![
                PermissionRuleValue::new("Bash", Some("P=build".to_string())),
                PermissionRuleValue::new("Bash", Some("npm run:*".to_string())),
            ],
        );
        let source = "P=build; NODE_OPTIONS=--inspect npm run \"$P\"";
        assert!(matches!(
            bash_tool_has_permission(source, &context, &cwd),
            PermissionResult::Ask { .. }
        ));
    }

    #[test]
    fn ast_paths_and_directory_changes_override_exact_allow() {
        let _project_dir = PinnedProjectDir::at_manifest_root();
        let cwd = std::env::current_dir().unwrap();
        let outside = std::env::temp_dir().join(format!(
            "nyan-ast-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        for source in [
            format!("echo x > {}", outside.display()),
            format!("cat < {}", outside.display()),
            "cat < -/../../nyan-ast-input-outside".to_string(),
            format!("P={} && cat \"$P\"", outside.display()),
            format!("rm \"$(cat <<'EOF'\n{}\nEOF\n)\"", outside.display()),
            "for file in Cargo.toml /etc/passwd; do cat \"$file\"; done".to_string(),
            "P=/etc/passwd; declare P=inside Q=\"$P\"; cat \"$Q\"".to_string(),
            "P=/etc/passwd; typeset P=inside Q=\"$P\"; cat \"$Q\"".to_string(),
            "P=/etc/passwd; export P=inside Q=\"$P\"; echo x > \"$Q\"".to_string(),
            "for file in inside /etc/nyan-ast-outside; do echo hello > \"$file\"; done".to_string(),
            "for dir in sub ..; do cd $dir; done".to_string(),
            "cd sub && echo x > file".to_string(),
            "cd sub && git status".to_string(),
            "cd sub; cd ..; pwd".to_string(),
        ] {
            let mut context = ToolPermissionContext::default();
            context.always_allow_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Bash", Some(source.clone()))],
            );
            let result = bash_tool_has_permission(&source, &context, &cwd);
            assert!(
                matches!(result, PermissionResult::Ask { .. }),
                "{source:?}: {result:?}"
            );
        }
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
                if message.contains("Bare command substitution") && suggestions.is_empty()
        ));
    }
}
