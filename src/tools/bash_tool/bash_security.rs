//! Maps to: CC `tools/BashTool/bashSecurity.ts`.
//!
//! Legacy regex security checks for Bash commands. The internal primary gate
//! lives in `utils/bash/ast.rs`; external/parser-unavailable paths use this
//! source-compatible fallback. The deprecated async wrapper still delegates to
//! this function because tree-sitter quote-context divergence telemetry is
//! intentionally omitted by project policy.

use crate::utils::bash::tree_sitter_analysis::TreeSitterAnalysis;
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuoteExtraction {
    pub with_double_quotes: String,
    pub fully_unquoted: String,
    pub unquoted_keep_quote_chars: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidationContext {
    original_command: String,
    base_command: String,
    unquoted_content: String,
    fully_unquoted_content: String,
    fully_unquoted_pre_strip: String,
    unquoted_keep_quote_chars: String,
    /// Maps to CC `ValidationContext.treeSitter`
    /// (`tools/BashTool/bashSecurity.ts:114-116`).
    tree_sitter: Option<TreeSitterAnalysis>,
}

static CONTROL_CHAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").expect("valid control char regex")
});
static INCOMPLETE_OPERATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(&&|\|\||;|>>?|<)").expect("valid operator regex"));
/// Maps to CC `COMMAND_SUBSTITUTION_PATTERNS`
/// (`tools/BashTool/bashSecurity.ts:16-41`). Order is load-bearing: the first
/// match wins, so it decides which message the user sees.
static COMMAND_SUBSTITUTION_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"<\(", "process substitution <()"),
        (r">\(", "process substitution >()"),
        (r"=\(", "Zsh process substitution =()"),
        (r"(?:^|[\s;&|])=[a-zA-Z_]", "Zsh equals expansion (=cmd)"),
        (r"\$\(", "$() command substitution"),
        (r"\$\{", "${} parameter substitution"),
        (r"\$\[", "$[] legacy arithmetic expansion"),
        (r"~\[", "Zsh-style parameter expansion"),
        (r"\(e:", "Zsh-style glob qualifiers"),
        (r"\(\+", "Zsh glob qualifier with command execution"),
        (
            r"\}\s*always\s*\{",
            "Zsh always block (try/always construct)",
        ),
        (r"<#", "PowerShell comment syntax"),
    ]
    .into_iter()
    .map(|(pattern, message)| {
        (
            Regex::new(pattern).expect("valid command substitution regex"),
            message,
        )
    })
    .collect()
});
static DANGEROUS_VARIABLE_1_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[<>|]\s*\$[A-Za-z_]").expect("valid dangerous variable regex"));
static DANGEROUS_VARIABLE_2_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$[A-Za-z_][A-Za-z0-9_]*\s*[|<>]").expect("valid dangerous variable regex")
});
static QUOTED_SHELL_META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|\s)["'][^"']*[;&][^"']*["'](\s|$)"#).expect("valid quoted shell meta regex")
});
static GLOB_META_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#"-name\s+["'][^"']*[;|&][^"']*["']"#,
        r#"-path\s+["'][^"']*[;|&][^"']*["']"#,
        r#"-iname\s+["'][^"']*[;|&][^"']*["']"#,
        r#"-regex\s+["'][^"']*[;&][^"']*["']"#,
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid glob meta regex"))
    .collect()
});
static JQ_SYSTEM_FUNCTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsystem\s*\(").expect("valid jq system regex"));
static JQ_FILE_ARGUMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)(?:-f\b|--from-file|--rawfile|--slurpfile|-L\b|--library-path)")
        .expect("valid jq file argument regex")
});
static IFS_INJECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$IFS|\$\{[^}]*IFS").expect("valid IFS injection regex"));
/// DEVIATION(SECURITY): CC `validateIFSInjection` only flags `$IFS` reads. This
/// keeps the pre-existing stricter assignment check so `IFS=,` still prompts.
static IFS_ASSIGNMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bIFS\s*=").expect("valid IFS assignment regex"));
static PROC_ENVIRON_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/proc/.*/environ").expect("valid proc environ regex"));
static BACKSLASH_ESCAPED_WHITESPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\[ \t]").expect("valid escaped whitespace regex"));
static BACKSLASH_ESCAPED_OPERATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\[;&|<>]").expect("valid escaped operator regex"));
static MID_WORD_HASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\S#").expect("valid mid-word hash regex"));
static BRACE_EXPANSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{[^}\n]*[,\.][^}\n]*\}").expect("valid brace expansion regex"));
const ZSH_DANGEROUS_COMMANDS: &[&str] = &[
    "zmodload", "emulate", "sysopen", "sysread", "syswrite", "sysseek", "zpty", "ztcp", "zsocket",
    "mapfile", "zf_rm", "zf_mv", "zf_ln", "zf_chmod", "zf_chown", "zf_mkdir", "zf_rmdir",
    "zf_chgrp",
];

/// Maps to CC local `extractQuotedContent(command, isJq)`.
pub fn extract_quoted_content(command: &str, is_jq: bool) -> QuoteExtraction {
    let mut with_double_quotes = String::new();
    let mut fully_unquoted = String::new();
    let mut unquoted_keep_quote_chars = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            escaped = false;
            if !in_single_quote {
                with_double_quotes.push(ch);
            }
            if !in_single_quote && !in_double_quote {
                fully_unquoted.push(ch);
                unquoted_keep_quote_chars.push(ch);
            }
            continue;
        }
        if ch == '\\' && !in_single_quote {
            escaped = true;
            if !in_single_quote {
                with_double_quotes.push(ch);
            }
            if !in_single_quote && !in_double_quote {
                fully_unquoted.push(ch);
                unquoted_keep_quote_chars.push(ch);
            }
            continue;
        }
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            unquoted_keep_quote_chars.push(ch);
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            unquoted_keep_quote_chars.push(ch);
            if !is_jq {
                continue;
            }
        }
        if !in_single_quote {
            with_double_quotes.push(ch);
        }
        if !in_single_quote && !in_double_quote {
            fully_unquoted.push(ch);
            unquoted_keep_quote_chars.push(ch);
        }
    }

    QuoteExtraction {
        with_double_quotes,
        fully_unquoted,
        unquoted_keep_quote_chars,
    }
}

/// Maps to CC local `stripSafeRedirections(content)`.
pub fn strip_safe_redirections(content: &str) -> String {
    static STDERR_TO_STDOUT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s+2\s*>\s*&\s*1(\s|$)").expect("valid fd redirection regex")
    });
    static DEVNULL_OUT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[012]?\s*>\s*/dev/null(\s|$)").expect("valid /dev/null out regex")
    });
    static DEVNULL_IN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s*<\s*/dev/null(\s|$)").expect("valid /dev/null in regex"));
    let value = STDERR_TO_STDOUT_RE.replace_all(content, "$1");
    let value = DEVNULL_OUT_RE.replace_all(&value, "$1");
    DEVNULL_IN_RE.replace_all(&value, "$1").to_string()
}

/// Maps to CC local `hasUnescapedChar(content, char)`.
pub fn has_unescaped_char(content: &str, target: char) -> bool {
    let mut chars = content.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == target {
            return true;
        }
    }
    false
}

/// Maps to CC `stripSafeHeredocSubstitutions(command)`.
pub fn strip_safe_heredoc_substitutions(command: &str) -> Option<String> {
    if !command.contains("$(") || !command.contains("<<") {
        return None;
    }
    let start = command.find("$(cat")?;
    let open = command[start..].find("<<")? + start;
    let after_operator = &command[open + 2..];
    let trimmed = after_operator.trim_start_matches([' ', '\t', '-']);
    let (delimiter, quoted_len) = if let Some(rest) = trimmed.strip_prefix('\'') {
        let end = rest.find('\'')?;
        (&rest[..end], end + 2)
    } else {
        let rest = trimmed.strip_prefix('\\')?;
        let end = rest
            .find(|ch: char| ch.is_whitespace())
            .unwrap_or(rest.len());
        (&rest[..end], end + 1)
    };
    if delimiter.is_empty()
        || !delimiter
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    let after_delim_start = trimmed.as_ptr() as usize - command.as_ptr() as usize + quoted_len;
    let after_delim = &command[after_delim_start..];
    let newline = after_delim.find('\n')?;
    if !after_delim[..newline]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t'))
    {
        return None;
    }
    let body_start = after_delim_start + newline + 1;
    let body = &command[body_start..];
    let mut offset = body_start;
    for line in body.split_inclusive('\n') {
        let raw = line.trim_end_matches('\n');
        let line_no_tabs = raw.trim_start_matches('\t');
        if line_no_tabs == delimiter {
            let after_line = &command[offset + line.len()..];
            let close_line = after_line.strip_prefix(')').or_else(|| {
                after_line
                    .strip_prefix('\n')
                    .and_then(|rest| rest.trim_start_matches([' ', '\t']).strip_prefix(')'))
            });
            if close_line.is_some() {
                let close_index = command[offset + line.len()..]
                    .find(')')
                    .map(|idx| offset + line.len() + idx + 1)?;
                return Some(format!("{}{}", &command[..start], &command[close_index..]));
            }
        }
        offset += line.len();
    }
    None
}

/// Maps to CC `hasSafeHeredocSubstitution(command)`.
pub fn has_safe_heredoc_substitution(command: &str) -> bool {
    strip_safe_heredoc_substitutions(command).is_some()
}

/// Maps to CC `bashCommandIsSafe_DEPRECATED(command)`.
pub fn bash_command_is_safe_deprecated(command: &str) -> PermissionResult {
    if CONTROL_CHAR_RE.is_match(command) {
        return ask(
            "Command contains non-printable control characters that could be used to bypass security checks",
        );
    }
    if crate::utils::bash::shell_quote::has_shell_quote_single_quote_bug(command) {
        return ask(
            "Command contains single-quoted backslash pattern that could bypass security checks",
        );
    }
    let processed_command = strip_quoted_heredoc_bodies(command);
    let base_command = command.split(' ').next().unwrap_or_default().to_string();
    let quote = extract_quoted_content(&processed_command, base_command == "jq");
    let context = ValidationContext {
        original_command: command.to_string(),
        base_command,
        unquoted_content: quote.with_double_quotes,
        fully_unquoted_content: strip_safe_redirections(&quote.fully_unquoted),
        fully_unquoted_pre_strip: quote.fully_unquoted,
        unquoted_keep_quote_chars: quote.unquoted_keep_quote_chars,
        tree_sitter: None,
    };

    run_security_validators(&context)
}

/// Maps to CC `bashCommandIsSafeAsync_DEPRECATED(command)`
/// (`tools/BashTool/bashSecurity.ts:2426-2592`). Tree-sitter quote-context
/// divergence telemetry is intentionally omitted by project policy.
pub async fn bash_command_is_safe_async_deprecated(command: &str) -> PermissionResult {
    let parsed = crate::utils::bash::parsed_command::ParsedCommand::parse(command);
    let Some(analysis) = parsed
        .as_ref()
        .and_then(|parsed| parsed.get_tree_sitter_analysis())
        .cloned()
    else {
        return bash_command_is_safe_deprecated(command);
    };

    if CONTROL_CHAR_RE.is_match(command) {
        return ask(
            "Command contains non-printable control characters that could be used to bypass security checks",
        );
    }
    if crate::utils::bash::shell_quote::has_shell_quote_single_quote_bug(command) {
        return ask(
            "Command contains single-quoted backslash pattern that could bypass security checks",
        );
    }

    let base_command = command.split(' ').next().unwrap_or_default().to_string();
    let quote = analysis.quote_context.clone();
    let context = ValidationContext {
        original_command: command.to_string(),
        base_command,
        unquoted_content: quote.with_double_quotes,
        fully_unquoted_content: strip_safe_redirections(&quote.fully_unquoted),
        fully_unquoted_pre_strip: quote.fully_unquoted,
        unquoted_keep_quote_chars: quote.unquoted_keep_quote_chars,
        tree_sitter: Some(analysis),
    };

    run_security_validators(&context)
}

fn run_security_validators(context: &ValidationContext) -> PermissionResult {
    for validator in [
        validate_empty,
        validate_incomplete_commands,
        validate_safe_command_substitution,
        validate_git_commit,
    ] {
        let result = validator(context);
        if matches!(result, PermissionResult::Allow { .. }) {
            return passthrough(match result {
                PermissionResult::Allow {
                    decision_reason: Some(PermissionDecisionReason::Other { reason }),
                    ..
                } => reason,
                _ => "Command allowed".to_string(),
            });
        }
        if !matches!(result, PermissionResult::Passthrough { .. }) {
            return result;
        }
    }

    let validators: &[fn(&ValidationContext) -> PermissionResult] = &[
        validate_jq_command,
        validate_obfuscated_flags,
        validate_shell_metacharacters,
        validate_dangerous_variables,
        validate_comment_quote_desync,
        validate_quoted_newline,
        validate_carriage_return,
        validate_newlines,
        validate_ifs_injection,
        validate_proc_environ_access,
        validate_dangerous_patterns,
        validate_redirections,
        validate_backslash_escaped_whitespace,
        validate_backslash_escaped_operators,
        validate_unicode_whitespace,
        validate_mid_word_hash,
        validate_brace_expansion,
        validate_zsh_dangerous_commands,
        validate_malformed_token_injection,
    ];

    let mut deferred_non_misparsing = None;
    for validator in validators {
        let result = validator(context);
        if matches!(result, PermissionResult::Ask { .. }) {
            if is_non_misparsing_validator(*validator) {
                if deferred_non_misparsing.is_none() {
                    deferred_non_misparsing = Some(result);
                }
                continue;
            }
            return result;
        }
    }
    deferred_non_misparsing
        .unwrap_or_else(|| passthrough("Command passed all security checks".to_string()))
}

fn validate_empty(context: &ValidationContext) -> PermissionResult {
    if context.original_command.trim().is_empty() {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({"command": context.original_command})),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Empty command is safe".to_string(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }
    passthrough("Command is not empty".to_string())
}

fn validate_incomplete_commands(context: &ValidationContext) -> PermissionResult {
    let trimmed = context.original_command.trim();
    if context.original_command.starts_with("\t") || context.original_command.starts_with(" \t") {
        return ask("Command appears to be an incomplete fragment (starts with tab)");
    }
    if trimmed.starts_with('-') {
        return ask("Command appears to be an incomplete fragment (starts with flags)");
    }
    if INCOMPLETE_OPERATOR_RE.is_match(&context.original_command) {
        return ask("Command appears to be a continuation line (starts with operator)");
    }
    passthrough("Command appears complete".to_string())
}

fn is_safe_heredoc_substitution(command: &str) -> bool {
    static SAFE_REMAINDER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"^[a-zA-Z0-9 \t\"'.\-/_@=,:+~]*$"#)
            .expect("valid safe heredoc remainder regex")
    });
    if command.matches("$(cat").count() != 1 {
        return false;
    }
    let Some(remaining) = strip_safe_heredoc_substitutions(command) else {
        return false;
    };
    if !SAFE_REMAINDER.is_match(&remaining) {
        return false;
    }
    let start = command.find("$(cat").unwrap_or_default();
    if !remaining.trim().is_empty() && command[..start].trim().is_empty() {
        return false;
    }
    matches!(
        bash_command_is_safe_deprecated(&remaining),
        PermissionResult::Passthrough { .. }
    )
}

fn validate_safe_command_substitution(context: &ValidationContext) -> PermissionResult {
    if !context.original_command.contains("$(") || !context.original_command.contains("<<") {
        return passthrough("No heredoc in substitution".to_string());
    }
    if is_safe_heredoc_substitution(&context.original_command) {
        return PermissionResult::Allow {
            updated_input: Some(serde_json::json!({"command": context.original_command})),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "Safe command substitution: cat with quoted/escaped heredoc delimiter"
                    .to_string(),
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        };
    }
    passthrough("Command substitution needs validation".to_string())
}

fn validate_git_commit(context: &ValidationContext) -> PermissionResult {
    if context.base_command == "git"
        && context.original_command.starts_with("git commit ")
        && (context.original_command.contains("$({")
            || context.original_command.contains("$(")
            || has_unescaped_char(&context.original_command, '`'))
    {
        return ask("Git commit message contains command substitution patterns");
    }
    passthrough("Not a git commit".to_string())
}

fn validate_jq_command(context: &ValidationContext) -> PermissionResult {
    if context.base_command != "jq" {
        return passthrough("Not jq".to_string());
    }
    if JQ_SYSTEM_FUNCTION_RE.is_match(&context.original_command) {
        return ask("jq command contains system() function which executes arbitrary commands");
    }
    let after_jq = context
        .original_command
        .get(3..)
        .unwrap_or_default()
        .trim()
        .to_string();
    if JQ_FILE_ARGUMENT_RE.is_match(&after_jq) {
        return ask(
            "jq command contains dangerous flags that could execute code or read arbitrary files",
        );
    }
    passthrough("jq command is safe".to_string())
}

fn validate_obfuscated_flags(context: &ValidationContext) -> PermissionResult {
    if context.fully_unquoted_content.contains("$'")
        || context.fully_unquoted_content.contains("\\x")
    {
        return ask("Command contains obfuscated flags or ANSI-C escape sequences");
    }
    passthrough("No obfuscated flags".to_string())
}

fn validate_shell_metacharacters(context: &ValidationContext) -> PermissionResult {
    let message = "Command contains shell metacharacters (;, |, or &) in arguments";
    if QUOTED_SHELL_META_RE.is_match(&context.unquoted_content)
        || GLOB_META_RES
            .iter()
            .any(|regex| regex.is_match(&context.unquoted_content))
    {
        return ask(message);
    }
    passthrough("No metacharacters".to_string())
}

fn validate_dangerous_variables(context: &ValidationContext) -> PermissionResult {
    if DANGEROUS_VARIABLE_1_RE.is_match(&context.fully_unquoted_content)
        || DANGEROUS_VARIABLE_2_RE.is_match(&context.fully_unquoted_content)
    {
        return ask("Command contains variables in dangerous contexts (redirections or pipes)");
    }
    passthrough("No dangerous variables".to_string())
}

fn validate_dangerous_patterns(context: &ValidationContext) -> PermissionResult {
    let unquoted = &context.unquoted_content;
    if has_unescaped_char(unquoted, '`') {
        return ask("Command contains backticks (`) for command substitution");
    }
    for (pattern, message) in COMMAND_SUBSTITUTION_PATTERNS.iter() {
        if pattern.is_match(unquoted) {
            return ask(format!("Command contains {message}"));
        }
    }
    passthrough("No dangerous patterns".to_string())
}

fn validate_redirections(context: &ValidationContext) -> PermissionResult {
    if context.fully_unquoted_content.contains('<') {
        return ask("Command contains input redirection (<) which could read sensitive files");
    }
    if context.fully_unquoted_content.contains('>') {
        return ask("Command contains output redirection (>) which could write to arbitrary files");
    }
    passthrough("No redirections".to_string())
}

fn validate_newlines(context: &ValidationContext) -> PermissionResult {
    if context
        .fully_unquoted_pre_strip
        .lines()
        .skip(1)
        .any(|line| !line.trim().is_empty())
    {
        return ask("Command contains newlines that could separate multiple commands");
    }
    passthrough("No newlines".to_string())
}

fn validate_carriage_return(context: &ValidationContext) -> PermissionResult {
    if context.fully_unquoted_pre_strip.contains('\r') {
        return ask(
            "Command contains carriage return (\\r) which shell-quote and bash tokenize differently",
        );
    }
    passthrough("No carriage returns".to_string())
}

fn validate_ifs_injection(context: &ValidationContext) -> PermissionResult {
    if IFS_INJECTION_RE.is_match(&context.original_command)
        || IFS_ASSIGNMENT_RE.is_match(&context.fully_unquoted_content)
    {
        return ask("Command contains IFS variable usage which could bypass security validation");
    }
    passthrough("No IFS injection detected".to_string())
}

fn validate_proc_environ_access(context: &ValidationContext) -> PermissionResult {
    if PROC_ENVIRON_RE.is_match(&context.original_command) {
        return ask(
            "Command accesses /proc/*/environ which could expose sensitive environment variables",
        );
    }
    passthrough("No /proc/environ access detected".to_string())
}

fn validate_backslash_escaped_whitespace(context: &ValidationContext) -> PermissionResult {
    if BACKSLASH_ESCAPED_WHITESPACE_RE.is_match(&context.fully_unquoted_pre_strip) {
        return ask(
            "Command contains backslash-escaped whitespace that could alter command parsing",
        );
    }
    passthrough("No backslash-escaped whitespace".to_string())
}

fn validate_backslash_escaped_operators(context: &ValidationContext) -> PermissionResult {
    if context
        .tree_sitter
        .as_ref()
        .is_some_and(|analysis| !analysis.has_actual_operator_nodes)
    {
        return passthrough("No operator nodes in AST".to_string());
    }

    if BACKSLASH_ESCAPED_OPERATOR_RE.is_match(&context.fully_unquoted_pre_strip) {
        return ask(
            "Command contains a backslash before a shell operator (;, |, &, <, >) which can hide command structure",
        );
    }
    passthrough("No backslash-escaped operators".to_string())
}

fn validate_unicode_whitespace(context: &ValidationContext) -> PermissionResult {
    if context
        .fully_unquoted_content
        .chars()
        .any(|ch| ch.is_whitespace() && !matches!(ch, ' ' | '\t' | '\n' | '\r'))
    {
        return ask(
            "Command contains Unicode whitespace characters that could cause parsing inconsistencies",
        );
    }
    passthrough("No Unicode whitespace".to_string())
}

fn validate_mid_word_hash(context: &ValidationContext) -> PermissionResult {
    if MID_WORD_HASH_RE.is_match(&context.unquoted_keep_quote_chars) {
        return ask(
            "Command contains mid-word # which is parsed differently by shell-quote vs bash",
        );
    }
    passthrough("No mid-word hash".to_string())
}

fn validate_brace_expansion(context: &ValidationContext) -> PermissionResult {
    if BRACE_EXPANSION_RE.is_match(&context.fully_unquoted_pre_strip) {
        return ask("Command contains brace expansion that could alter command parsing");
    }
    passthrough("No brace expansion".to_string())
}

fn validate_zsh_dangerous_commands(context: &ValidationContext) -> PermissionResult {
    let base = context
        .fully_unquoted_content
        .split_whitespace()
        .next()
        .unwrap_or_default();
    if ZSH_DANGEROUS_COMMANDS.contains(&base) {
        return ask(format!(
            "Command uses Zsh-specific '{base}' which can bypass security checks"
        ));
    }
    passthrough("No Zsh dangerous commands".to_string())
}

fn validate_comment_quote_desync(context: &ValidationContext) -> PermissionResult {
    if context.tree_sitter.is_some() {
        return passthrough("Tree-sitter quote context is authoritative".to_string());
    }

    let chars = context.original_command.chars().collect::<Vec<_>>();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if single {
            if character == '\'' {
                single = false;
            }
            index += 1;
            continue;
        }
        if character == '\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if double {
            if character == '"' {
                double = false;
            }
            index += 1;
            continue;
        }
        if character == '\'' {
            single = true;
            index += 1;
            continue;
        }
        if character == '"' {
            double = true;
            index += 1;
            continue;
        }
        if character == '#' {
            let end = chars[index + 1..]
                .iter()
                .position(|character| *character == '\n')
                .map(|offset| index + 1 + offset)
                .unwrap_or(chars.len());
            if chars[index + 1..end]
                .iter()
                .any(|character| matches!(character, '\'' | '"'))
            {
                return ask(
                    "Command contains quote characters inside a # comment which can desync quote tracking",
                );
            }
            index = end.saturating_add(1);
            continue;
        }
        index += 1;
    }
    passthrough("No comment quote desync".to_string())
}

fn validate_quoted_newline(context: &ValidationContext) -> PermissionResult {
    let command = &context.original_command;
    if !command.contains('\n') || !command.contains('#') {
        return passthrough("No newline or no hash".to_string());
    }
    let chars = command.chars().collect::<Vec<_>>();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && !single {
            escaped = true;
            continue;
        }
        if character == '\'' && !double {
            single = !single;
            continue;
        }
        if character == '"' && !single {
            double = !double;
            continue;
        }
        if character == '\n' && (single || double) {
            let next = chars[index + 1..]
                .iter()
                .copied()
                .take_while(|character| *character != '\n')
                .collect::<String>();
            if next.trim().starts_with('#') {
                return ask(
                    "Command contains a quoted newline followed by a #-prefixed line, which can hide arguments from line-based permission checks",
                );
            }
        }
    }
    passthrough("No quoted newline-hash pattern".to_string())
}

fn validate_malformed_token_injection(context: &ValidationContext) -> PermissionResult {
    if crate::utils::bash::shell_quote::has_malformed_syntax(&context.original_command) {
        ask(
            "Command contains ambiguous syntax with command separators that could be misinterpreted",
        )
    } else {
        passthrough("No malformed token injection".to_string())
    }
}

fn is_non_misparsing_validator(validator: fn(&ValidationContext) -> PermissionResult) -> bool {
    std::ptr::fn_addr_eq(
        validator,
        validate_newlines as fn(&ValidationContext) -> PermissionResult,
    ) || std::ptr::fn_addr_eq(
        validator,
        validate_redirections as fn(&ValidationContext) -> PermissionResult,
    )
}

fn strip_quoted_heredoc_bodies(command: &str) -> String {
    crate::utils::bash::heredoc::strip_quoted_heredoc_bodies(command)
}

fn ask(message: impl Into<String>) -> PermissionResult {
    let reason = message.into();
    PermissionResult::Ask {
        message: reason.clone(),
        updated_input: None,
        decision_reason: Some(PermissionDecisionReason::SafetyCheck {
            reason,
            classifier_approvable: true,
        }),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_bash_security_check_for_misparsing: false,
        pending_classifier_check: None,
        content_blocks: Vec::new(),
    }
}

fn passthrough(message: impl Into<String>) -> PermissionResult {
    PermissionResult::Passthrough {
        message: message.into(),
        decision_reason: None,
        suggestions: Vec::new(),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_content_and_safe_redirection_helpers_match_official_shapes() {
        let quoted = extract_quoted_content(r#"echo 'hidden' "$VISIBLE" tail"#, false);
        assert_eq!(quoted.with_double_quotes, "echo  $VISIBLE tail");
        assert_eq!(quoted.fully_unquoted, "echo   tail");
        assert_eq!(
            strip_safe_redirections("echo hi > /dev/null 2>&1"),
            "echo hi"
        );
        assert!(!has_unescaped_char(r"escaped \` tick", '`'));
        assert!(has_unescaped_char("`tick`", '`'));
    }

    #[test]
    fn safe_heredoc_substitution_detection_is_conservative() {
        let command = "printf %s $(cat <<'EOF'\nhello\nEOF\n)";
        assert!(has_safe_heredoc_substitution(command));
        assert_eq!(
            strip_safe_heredoc_substitutions(command).as_deref(),
            Some("printf %s ")
        );
        assert!(!has_safe_heredoc_substitution("$(cat <<EOF\n$(id)\nEOF\n)"));
    }

    #[test]
    fn legacy_security_matches_official_quoted_and_heredoc_edge_cases() {
        for command in [
            "echo {foo",
            "echo foo)",
            "echo a\\",
            "echo \"unterminated",
            "echo {\"hi\":\"hi;evil\"}",
            "echo foo(bar; cat",
            "cat <<'EOF'\n$(id)\nEOF",
        ] {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Passthrough { .. }
                ),
                "command={command:?}"
            );
        }
        for command in [
            "echo \"hi;evil | cat",
            "echo {foo; cat",
            "echo foo\\ bar",
            "echo foo\\;bar",
        ] {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Ask { .. }
                ),
                "command={command:?}"
            );
        }
        assert!(matches!(
            bash_command_is_safe_deprecated("echo $(cat <<'EOF'\nhello\nEOF\n)"),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Safe command substitution: cat with quoted/escaped heredoc delimiter"
        ));
        assert!(matches!(
            bash_command_is_safe_deprecated("printf %s $(cat <<'EOF'\nhello\nEOF\n)"),
            PermissionResult::Ask { .. }
        ));
    }

    #[test]
    fn legacy_security_matches_cc_2_1_88_bun_differential_matrix() {
        let safe = [
            "echo hello",
            "echo \"hello world\"",
            "echo 'hello world'",
            "echo \"unterminated",
            "echo 'unterminated",
            "echo foo\\",
            "echo \"x; rm -rf /\"",
            "echo 'x | cat'",
            "echo x; cat file",
            "echo x && cat file",
            "echo x || cat file",
            "echo x | cat",
            "echo x & cat file",
            "echo $HOME",
            "echo *.txt",
            "cat <<'EOF'\nhello; $(id)\nEOF",
            "echo hi 2>&1",
            "printf \"a\\nb\"",
            "echo \"a\nb\"",
            "echo\u{200b}hello",
            "echo foo(bar; cat",
            "echo {\"hi\":\"hi;evil\"}",
            "echo '{\"hi\":\"safe\"}'",
            "echo 'safe $(printf x)'",
            "FOO=bar echo \"$FOO\"",
            "x=1; echo \"$x\"",
            "if true; then echo ok; fi",
            "for x in a b; do echo \"$x\"; done",
            "true # comment ; rm -rf /",
            "echo \"# not comment; |\"",
            "echo \"quoted\\\nnewline\"",
        ];
        let ask = [
            "echo foo\\ bar",
            "echo foo\\\nbar",
            "echo $(id)",
            "echo `id`",
            "cat <(printf x)",
            "printf x > >(cat)",
            "echo ${HOME}",
            "echo $((1+2))",
            "echo {a,b}",
            "cat <<EOF\nhello; $(id)\nEOF",
            "cat <<-EOF\n\thello | cat\n\tEOF",
            "cat <<EOF\nunterminated",
            "cat <<< \"hello\"",
            "echo hi > out",
            "echo hi 2>>err",
            "cat < input",
            "echo hi &>out",
            "echo hi >|out",
            "echo\u{00a0}hello",
            "echo\u{2028}hello",
            "echo {foo; cat",
            "echo \"hi;evil | cat",
            "echo \"safe $(printf x)\"",
            "true\n# comment with \" and ;\necho ok",
            "echo \\; literal",
            "echo \\| literal",
            "echo \\& literal",
        ];
        for command in safe {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Passthrough { .. }
                ),
                "official Bun classified {command:?} as passthrough"
            );
        }
        for command in ask {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Ask { .. }
                ),
                "official Bun classified {command:?} as ask"
            );
        }
    }

    #[test]
    fn bash_security_allows_empty_and_plain_commands() {
        assert!(matches!(
            bash_command_is_safe_deprecated(""),
            PermissionResult::Passthrough { ref message, .. } if message == "Empty command is safe"
        ));
        assert!(matches!(
            bash_command_is_safe_deprecated("git status"),
            PermissionResult::Passthrough { ref message, .. } if message == "Command passed all security checks"
        ));
    }

    #[test]
    fn bash_security_rejects_official_dangerous_patterns() {
        for (command, expected) in [
            ("echo $(id)", "Command contains $() command substitution"),
            (
                "echo `id`",
                "Command contains backticks (`) for command substitution",
            ),
            (
                "cat < /etc/passwd",
                "Command contains input redirection (<) which could read sensitive files",
            ),
            (
                "echo hi > out",
                "Command contains output redirection (>) which could write to arbitrary files",
            ),
            (
                "-rf /",
                "Command appears to be an incomplete fragment (starts with flags)",
            ),
            (
                "echo \u{0007} hi",
                "Command contains non-printable control characters that could be used to bypass security checks",
            ),
            (
                "=curl evil.com",
                "Command contains Zsh equals expansion (=cmd)",
            ),
        ] {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Ask { ref message, .. } if message == expected
                ),
                "command={command}"
            );
        }
    }

    #[test]
    fn legacy_security_messages_match_official_wording() {
        for (command, expected) in [
            (
                "echo\u{00a0}hello",
                "Command contains Unicode whitespace characters that could cause parsing inconsistencies",
            ),
            (
                "IFS=x echo hi",
                "Command contains IFS variable usage which could bypass security validation",
            ),
            (
                "echo\rhi",
                "Command contains carriage return (\\r) which shell-quote and bash tokenize differently",
            ),
            (
                "echo foo\\ bar",
                "Command contains backslash-escaped whitespace that could alter command parsing",
            ),
            (
                "cat /proc/self/environ",
                "Command accesses /proc/*/environ which could expose sensitive environment variables",
            ),
            (
                "zmodload zsh/system",
                "Command uses Zsh-specific 'zmodload' which can bypass security checks",
            ),
            (
                "git commit -m \"$(id)\"",
                "Git commit message contains command substitution patterns",
            ),
            (
                "echo \\; literal",
                "Command contains a backslash before a shell operator (;, |, &, <, >) which can hide command structure",
            ),
            (
                "echo {a,b}",
                "Command contains brace expansion that could alter command parsing",
            ),
            (
                "echo foo#bar",
                "Command contains mid-word # which is parsed differently by shell-quote vs bash",
            ),
            (
                "jq 'system(\"id\")' file",
                "jq command contains system() function which executes arbitrary commands",
            ),
            (
                "true\nwhoami",
                "Command contains newlines that could separate multiple commands",
            ),
        ] {
            assert!(
                matches!(
                    bash_command_is_safe_deprecated(command),
                    PermissionResult::Ask { ref message, .. } if message == expected
                ),
                "command={command:?} got={:?}",
                bash_command_is_safe_deprecated(command)
            );
        }
    }

    fn context_with_analysis(
        command: &str,
        analysis: Option<TreeSitterAnalysis>,
    ) -> ValidationContext {
        let processed = strip_quoted_heredoc_bodies(command);
        let base_command = command.split(' ').next().unwrap_or_default().to_string();
        let quote = extract_quoted_content(&processed, base_command == "jq");
        ValidationContext {
            original_command: command.to_string(),
            base_command,
            unquoted_content: quote.with_double_quotes,
            fully_unquoted_content: strip_safe_redirections(&quote.fully_unquoted),
            fully_unquoted_pre_strip: quote.fully_unquoted,
            unquoted_keep_quote_chars: quote.unquoted_keep_quote_chars,
            tree_sitter: analysis,
        }
    }

    #[test]
    fn tree_sitter_context_short_circuits_the_two_ast_authoritative_validators() {
        let escaped_operator = "find . -exec cat {} \\;";
        assert!(matches!(
            validate_backslash_escaped_operators(&context_with_analysis(escaped_operator, None)),
            PermissionResult::Ask { .. }
        ));
        assert!(matches!(
            validate_backslash_escaped_operators(&context_with_analysis(
                escaped_operator,
                Some(TreeSitterAnalysis::default())
            )),
            PermissionResult::Passthrough { ref message, .. } if message == "No operator nodes in AST"
        ));
        assert!(matches!(
            validate_backslash_escaped_operators(&context_with_analysis(
                escaped_operator,
                Some(TreeSitterAnalysis {
                    has_actual_operator_nodes: true,
                    ..TreeSitterAnalysis::default()
                })
            )),
            PermissionResult::Ask { .. }
        ));

        let desync = "true\n# comment with \" and ;\necho ok";
        assert!(matches!(
            validate_comment_quote_desync(&context_with_analysis(desync, None)),
            PermissionResult::Ask { .. }
        ));
        assert!(matches!(
            validate_comment_quote_desync(&context_with_analysis(
                desync,
                Some(TreeSitterAnalysis::default())
            )),
            PermissionResult::Passthrough { ref message, .. }
                if message == "Tree-sitter quote context is authoritative"
        ));
    }

    #[test]
    fn async_path_lets_tree_sitter_clear_regex_only_false_positives() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");

        // Regex-only path asks on the escaped `;` of an `-exec` clause.
        assert!(matches!(
            bash_command_is_safe_deprecated("find . -name '*.rs' -exec cat {} \\;"),
            PermissionResult::Ask { .. }
        ));

        let with_ast = runtime.block_on(bash_command_is_safe_async_deprecated(
            "find . -name '*.rs' -exec cat {} \\;",
        ));
        if crate::utils::bash::parsed_command::ParsedCommand::parse(
            "find . -name '*.rs' -exec cat {} \\;",
        )
        .and_then(|parsed| parsed.get_tree_sitter_analysis().cloned())
        .is_some()
        {
            assert!(
                matches!(with_ast, PermissionResult::Passthrough { .. }),
                "tree-sitter should clear the escaped-operator false positive, got {with_ast:?}"
            );
        } else {
            assert!(matches!(with_ast, PermissionResult::Ask { .. }));
        }
    }
}
