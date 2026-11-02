//! Maps to: CC `tools/BashTool/sedValidation.ts`.
//!
//! Cross-cutting sed allowlist/denylist validation. This module intentionally
//! returns `ask` rather than trying to execute or persist edits; execution-time
//! `_simulatedSedEdit` handling stays in `BashTool.tsx`/`mod.rs` parity code.

use crate::tool::ToolPermissionContext;
use crate::types::permissions::PermissionMode;
use crate::utils::permissions::permission_result::{PermissionDecisionReason, PermissionResult};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SedValidationOptions {
    pub allow_file_writes: bool,
}

/// Maps to CC local `validateFlagsAgainstAllowlist(flags, allowedFlags)`.
fn validate_flags_against_allowlist(flags: &[String], allowed_flags: &[&str]) -> bool {
    for flag in flags {
        if flag.starts_with('-') && !flag.starts_with("--") && flag.chars().count() > 2 {
            for ch in flag.chars().skip(1) {
                let single_flag = format!("-{ch}");
                if !allowed_flags.contains(&single_flag.as_str()) {
                    return false;
                }
            }
        } else if !allowed_flags.contains(&flag.as_str()) {
            return false;
        }
    }
    true
}

/// Maps to CC `isLinePrintingCommand(command, expressions)`.
pub fn is_line_printing_command(command: &str, expressions: &[String]) -> bool {
    let Some(without_sed) = without_sed_prefix(command) else {
        return false;
    };
    let Ok(parsed) = parse_shell_words(without_sed) else {
        return false;
    };
    let flags = parsed
        .iter()
        .filter(|arg| arg.starts_with('-') && arg.as_str() != "--")
        .cloned()
        .collect::<Vec<_>>();

    let allowed_flags = [
        "-n",
        "--quiet",
        "--silent",
        "-E",
        "--regexp-extended",
        "-r",
        "-z",
        "--zero-terminated",
        "--posix",
    ];
    if !validate_flags_against_allowlist(&flags, &allowed_flags) {
        return false;
    }

    let has_n_flag = flags.iter().any(|flag| {
        flag == "-n"
            || flag == "--quiet"
            || flag == "--silent"
            || (flag.starts_with('-') && !flag.starts_with("--") && flag.contains('n'))
    });
    if !has_n_flag || expressions.is_empty() {
        return false;
    }

    expressions
        .iter()
        .flat_map(|expr| expr.split(';'))
        .all(|cmd| is_print_command(cmd.trim()))
}

/// Maps to CC `isPrintCommand(cmd)`.
pub fn is_print_command(cmd: &str) -> bool {
    static PRINT_COMMAND_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:\d+|\d+,\d+)?p$").expect("valid sed print regex"));
    !cmd.is_empty() && PRINT_COMMAND_RE.is_match(cmd)
}

/// Maps to CC local `isSubstitutionCommand(...)`.
fn is_substitution_command(
    command: &str,
    expressions: &[String],
    has_file_arguments: bool,
    options: SedValidationOptions,
) -> bool {
    if !options.allow_file_writes && has_file_arguments {
        return false;
    }

    let Some(without_sed) = without_sed_prefix(command) else {
        return false;
    };
    let Ok(parsed) = parse_shell_words(without_sed) else {
        return false;
    };
    let flags = parsed
        .iter()
        .filter(|arg| arg.starts_with('-') && arg.as_str() != "--")
        .cloned()
        .collect::<Vec<_>>();

    let mut allowed_flags = vec!["-E", "--regexp-extended", "-r", "--posix"];
    if options.allow_file_writes {
        allowed_flags.push("-i");
        allowed_flags.push("--in-place");
    }
    if !validate_flags_against_allowlist(&flags, &allowed_flags) {
        return false;
    }

    let [expr] = expressions else {
        return false;
    };
    let expr = expr.trim();
    if !expr.starts_with('s') {
        return false;
    }
    let Some(rest) = expr.strip_prefix("s/") else {
        return false;
    };

    let mut delimiter_count = 0usize;
    let mut last_delimiter_byte = None;
    let mut chars = rest.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == '/' {
            delimiter_count += 1;
            last_delimiter_byte = Some(index);
        }
    }
    if delimiter_count != 2 {
        return false;
    }
    let expr_flags = &rest[last_delimiter_byte.unwrap_or(0) + 1..];
    sed_substitution_flags_are_allowed(expr_flags)
}

fn sed_substitution_flags_are_allowed(flags: &str) -> bool {
    let mut digit_count = 0usize;
    for ch in flags.chars() {
        match ch {
            'g' | 'p' | 'i' | 'm' | 'I' | 'M' => {}
            '1'..='9' => digit_count += 1,
            _ => return false,
        }
        if digit_count > 1 {
            return false;
        }
    }
    true
}

/// Maps to CC `sedCommandIsAllowedByAllowlist(command, options)`.
pub fn sed_command_is_allowed_by_allowlist(command: &str, options: SedValidationOptions) -> bool {
    let Ok(expressions) = extract_sed_expressions(command) else {
        return false;
    };
    let has_file_arguments = has_file_args(command);

    let (is_pattern1, is_pattern2) = if options.allow_file_writes {
        (
            false,
            is_substitution_command(command, &expressions, has_file_arguments, options),
        )
    } else {
        (
            is_line_printing_command(command, &expressions),
            is_substitution_command(command, &expressions, has_file_arguments, options),
        )
    };

    if !is_pattern1 && !is_pattern2 {
        return false;
    }
    if is_pattern2 && expressions.iter().any(|expr| expr.contains(';')) {
        return false;
    }
    !expressions
        .iter()
        .any(|expr| contains_dangerous_operations(expr))
}

/// Maps to CC `hasFileArgs(command)`.
pub fn has_file_args(command: &str) -> bool {
    let Some(without_sed) = without_sed_prefix(command) else {
        return false;
    };
    let Ok(parsed) = parse_shell_words(without_sed) else {
        return true;
    };

    let mut arg_count = 0usize;
    let mut has_e_flag = false;
    let mut index = 0usize;
    while index < parsed.len() {
        let arg = &parsed[index];

        if (arg == "-e" || arg == "--expression") && index + 1 < parsed.len() {
            has_e_flag = true;
            index += 2;
            continue;
        }
        if arg.starts_with("--expression=") || arg.starts_with("-e=") {
            has_e_flag = true;
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }

        arg_count += 1;
        if has_e_flag || arg_count > 1 {
            return true;
        }
        index += 1;
    }

    false
}

/// Maps to CC `extractSedExpressions(command)`.
pub fn extract_sed_expressions(command: &str) -> Result<Vec<String>, String> {
    let mut expressions = Vec::new();
    let Some(without_sed) = without_sed_prefix(command) else {
        return Ok(expressions);
    };

    static DANGEROUS_EW_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"-e[wWe]|-w[eE]").expect("valid sed dangerous flag-combo regex")
    });
    if DANGEROUS_EW_RE.is_match(without_sed) {
        return Err("Dangerous flag combination detected".to_string());
    }

    let parsed = parse_shell_words(without_sed)
        .map_err(|error| format!("Malformed shell syntax: {error}"))?;
    let mut found_e_flag = false;
    let mut found_expression = false;
    let mut index = 0usize;
    while index < parsed.len() {
        let arg = &parsed[index];

        if (arg == "-e" || arg == "--expression") && index + 1 < parsed.len() {
            found_e_flag = true;
            expressions.push(parsed[index + 1].clone());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--expression=") {
            found_e_flag = true;
            expressions.push(value.to_string());
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-e=") {
            found_e_flag = true;
            expressions.push(value.to_string());
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        if !found_e_flag && !found_expression {
            expressions.push(arg.clone());
            found_expression = true;
            index += 1;
            continue;
        }
        break;
    }

    Ok(expressions)
}

/// Maps to CC local `containsDangerousOperations(expression)`.
fn contains_dangerous_operations(expression: &str) -> bool {
    let cmd = expression.trim();
    if cmd.is_empty() {
        return false;
    }

    if cmd.chars().any(|ch| !ch.is_ascii() || ch == '\0') {
        return true;
    }
    if cmd.contains('{') || cmd.contains('}') || cmd.contains('\n') {
        return true;
    }
    if let Some(hash_index) = cmd.find('#')
        && (hash_index == 0 || cmd.as_bytes().get(hash_index - 1) != Some(&b's'))
    {
        return true;
    }

    if cmd.starts_with('!') || regex_is_match(&NEGATION_RE, cmd) {
        return true;
    }
    if regex_is_match(&GNU_STEP_ADDRESS_RE, cmd)
        || cmd.starts_with(',')
        || regex_is_match(&COMMA_OFFSET_RE, cmd)
    {
        return true;
    }
    if cmd.contains("s\\") || regex_is_match(&BACKSLASH_TRICK_RE, cmd) {
        return true;
    }
    if escaped_slash_then_write(cmd) || regex_is_match(&SLASH_SPACE_DANGEROUS_RE, cmd) {
        return true;
    }
    if cmd.starts_with("s/") && !regex_is_match(&STRICT_SLASH_SUBSTITUTION_RE, cmd) {
        return true;
    }
    if cmd.starts_with('s')
        && cmd
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, 'w' | 'W' | 'e' | 'E'))
        && !proper_substitution_without_dangerous_flags(cmd)
    {
        return true;
    }
    if WRITE_COMMAND_RES.iter().any(|regex| regex.is_match(cmd))
        || EXEC_COMMAND_RES.iter().any(|regex| regex.is_match(cmd))
    {
        return true;
    }
    if let Some(flags) = parse_substitution_flags_any_delimiter(cmd)
        && flags.chars().any(|ch| matches!(ch, 'w' | 'W' | 'e' | 'E'))
    {
        return true;
    }
    if parse_y_command_delimiter(cmd).is_some()
        && cmd.chars().any(|ch| matches!(ch, 'w' | 'W' | 'e' | 'E'))
    {
        return true;
    }

    false
}

static NEGATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[/\d$]!").expect("valid negation regex"));
static GNU_STEP_ADDRESS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d\s*~\s*\d|,\s*~\s*\d|\$\s*~\s*\d").expect("valid GNU sed step regex")
});
static COMMA_OFFSET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r",\s*[+-]").expect("valid sed offset regex"));
static BACKSLASH_TRICK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\[|#%@]").expect("valid backslash trick regex"));
static SLASH_SPACE_DANGEROUS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/[^/]*\s+[wWeE]").expect("valid slash-space dangerous regex"));
static STRICT_SLASH_SUBSTITUTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^s/[^/]*/[^/]*/[^/]*$").expect("valid strict sed substitution regex")
});
static WRITE_COMMAND_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^[wW]\s*\S+",
        r"^\d+\s*[wW]\s*\S+",
        r"^\$\s*[wW]\s*\S+",
        r"^/[^/]*/[IMim]*\s*[wW]\s*\S+",
        r"^\d+,\d+\s*[wW]\s*\S+",
        r"^\d+,\$\s*[wW]\s*\S+",
        r"^/[^/]*/[IMim]*,/[^/]*/[IMim]*\s*[wW]\s*\S+",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid sed write regex"))
    .collect()
});
static EXEC_COMMAND_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^e",
        r"^\d+\s*e",
        r"^\$\s*e",
        r"^/[^/]*/[IMim]*\s*e",
        r"^\d+,\d+\s*e",
        r"^\d+,\$\s*e",
        r"^/[^/]*/[IMim]*,/[^/]*/[IMim]*\s*e",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid sed execute regex"))
    .collect()
});

fn regex_is_match(regex: &LazyLock<Regex>, value: &str) -> bool {
    regex.is_match(value)
}

fn escaped_slash_then_write(cmd: &str) -> bool {
    cmd.find("\\/")
        .is_some_and(|index| cmd[index + 2..].chars().any(|ch| matches!(ch, 'w' | 'W')))
}

fn proper_substitution_without_dangerous_flags(cmd: &str) -> bool {
    parse_substitution_flags_any_delimiter(cmd)
        .is_some_and(|flags| !flags.chars().any(|ch| matches!(ch, 'w' | 'W' | 'e' | 'E')))
}

fn parse_substitution_flags_any_delimiter(cmd: &str) -> Option<&str> {
    let mut chars = cmd.char_indices();
    let (_, first) = chars.next()?;
    if first != 's' {
        return None;
    }
    let (delimiter_index, delimiter) = chars.next()?;
    if delimiter == '\\' || delimiter == '\n' {
        return None;
    }
    let mut seen = 0usize;
    let mut escaped = false;
    for (index, ch) in cmd[delimiter_index + delimiter.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == delimiter {
            seen += 1;
            if seen == 2 {
                let absolute_index = delimiter_index + delimiter.len_utf8() + index;
                return Some(&cmd[absolute_index + delimiter.len_utf8()..]);
            }
        }
    }
    None
}

fn parse_y_command_delimiter(cmd: &str) -> Option<char> {
    let mut chars = cmd.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == 'y' {
            let delimiter = *chars.peek()?;
            if delimiter != '\\' && delimiter != '\n' {
                return Some(delimiter);
            }
        }
    }
    None
}

/// Maps to CC `checkSedConstraints(input, toolPermissionContext)`.
pub fn check_sed_constraints(
    command: &str,
    tool_permission_context: &ToolPermissionContext,
) -> PermissionResult {
    for cmd in crate::utils::bash::commands::split_command_deprecated(command) {
        let trimmed = cmd.trim();
        let base_cmd = trimmed.split_whitespace().next().unwrap_or_default();
        if base_cmd != "sed" {
            continue;
        }
        let allow_file_writes = tool_permission_context.mode == PermissionMode::AcceptEdits;
        if !sed_command_is_allowed_by_allowlist(trimmed, SedValidationOptions { allow_file_writes })
        {
            return PermissionResult::Ask {
                message: "sed command requires approval (contains potentially dangerous operations)"
                    .to_string(),
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "sed command contains operations that require explicit approval (e.g., write commands, execute commands)"
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
    }

    PermissionResult::Passthrough {
        message: "No dangerous sed operations detected".to_string(),
        decision_reason: None,
        suggestions: Vec::new(),
        blocked_path: None,
        pending_classifier_check: None,
    }
}

fn without_sed_prefix(command: &str) -> Option<&str> {
    let trimmed = command.trim_start();
    let without_sed = trimmed.strip_prefix("sed")?;
    if !without_sed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_whitespace())
    {
        return None;
    }
    Some(without_sed.trim_start())
}

fn parse_shell_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    let mut in_word = false;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(ch);
                }
            }
            _ if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                in_word = true;
            }
            _ if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            _ if matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')') => {
                return Err(format!("unexpected operator {ch}"));
            }
            _ if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    in_word = true;
                }
            }
            _ => {
                current.push(ch);
                in_word = true;
            }
        }
    }

    if quote.is_some() {
        return Err("unterminated quote".to_string());
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_printing_allowlist_matches_official_strict_forms() {
        let expressions = vec!["1p;2,4p;p".to_string()];
        assert!(is_line_printing_command(
            "sed -nE '1p;2,4p;p' file.txt",
            &expressions
        ));
        assert!(is_print_command("10,200p"));
        assert!(!is_print_command("1w /tmp/out"));
        assert!(!is_line_printing_command(
            "sed -e '1p' file.txt",
            &expressions
        ));
    }

    #[test]
    fn substitution_allowlist_distinguishes_stdout_and_file_write_modes() {
        assert!(sed_command_is_allowed_by_allowlist(
            "sed 's/foo/bar/g'",
            SedValidationOptions::default()
        ));
        assert!(!sed_command_is_allowed_by_allowlist(
            "sed 's/foo/bar/g' file.txt",
            SedValidationOptions::default()
        ));
        assert!(sed_command_is_allowed_by_allowlist(
            "sed -i 's/foo/bar/g' file.txt",
            SedValidationOptions {
                allow_file_writes: true
            }
        ));
        assert!(!sed_command_is_allowed_by_allowlist(
            "sed -i 's/foo/bar/w /tmp/out' file.txt",
            SedValidationOptions {
                allow_file_writes: true
            }
        ));
    }

    #[test]
    fn extraction_and_file_arg_detection_match_official_edge_cases() {
        assert_eq!(
            extract_sed_expressions("sed -n -e '1p' -- file.txt").unwrap(),
            vec!["1p".to_string()]
        );
        assert!(has_file_args("sed -e 's/a/b/' file.txt"));
        assert!(has_file_args("sed 's/a/b/' file.txt"));
        assert!(!has_file_args("sed 's/a/b/'"));
        assert!(extract_sed_expressions("sed -ew file").is_err());
    }

    #[test]
    fn dangerous_sed_operations_are_rejected() {
        for command in [
            "sed -n '1w /tmp/out' file.txt",
            "sed 'e whoami'",
            "sed 's/foo/bar/e'",
            "sed 's/foo/bar/w /tmp/out'",
            "sed 's/foo/bar/;w /tmp/out'",
            "sed '1~2p'",
            "sed 's/foo/bar/ｗ'",
        ] {
            assert!(
                !sed_command_is_allowed_by_allowlist(command, SedValidationOptions::default()),
                "expected rejection: {command}"
            );
        }
    }

    #[test]
    fn check_sed_constraints_returns_official_permission_result_shapes() {
        let default_context = ToolPermissionContext::default();
        assert!(matches!(
            check_sed_constraints("echo hi && sed 's/foo/bar/'", &default_context),
            PermissionResult::Passthrough { ref message, .. }
                if message == "No dangerous sed operations detected"
        ));
        assert!(matches!(
            check_sed_constraints("sed -i 's/foo/bar/' file.txt", &default_context),
            PermissionResult::Ask { ref message, decision_reason: Some(PermissionDecisionReason::Other { .. }), .. }
                if message == "sed command requires approval (contains potentially dangerous operations)"
        ));
        let accept_edits = ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            check_sed_constraints("sed -i 's/foo/bar/' file.txt", &accept_edits),
            PermissionResult::Passthrough { .. }
        ));
    }
}
