//! Bash command splitting helpers.
//!
//! Maps to: CC `utils/bash/commands.ts`.
//!
//! Owns command splitting, legacy compound classification, output-redirection
//! extraction, and help-command detection. Prefix-model extraction remains a
//! separate unported consumer slice.

use super::shell_quote::ParseEntry;

pub const ALL_SUPPORTED_CONTROL_OPERATORS: &[&str] = &["&&", "||", ";", ";;", "|", ">&", ">", ">>"];

fn flush_word(word: &mut String, command_words: &mut Vec<String>) {
    if !word.is_empty() {
        command_words.push(std::mem::take(word));
    }
}

fn flush_command(command_words: &mut Vec<String>, parts: &mut Vec<String>) {
    if !command_words.is_empty() {
        parts.push(std::mem::take(command_words).join(" "));
    }
}

/// Maps to CC `splitCommandWithOperators(command)`. The TS implementation uses
/// shell-quote then collapses adjacent string/glob tokens; this native scanner
/// preserves the resulting quote/operator boundaries directly.
pub fn split_command_with_operators(command: &str) -> Vec<String> {
    let original = join_continuation_lines(command);
    let extraction = super::heredoc::extract_heredocs(&original, false);
    if original.contains("<<") && extraction.heredocs.is_empty() {
        // Extraction ambiguity follows the source's safe fallback direction.
        return vec![original];
    }
    let command = extraction.processed_command.as_str();
    let chars = command.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut command_words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut index = 0usize;

    while index < chars.len() {
        let character = chars[index];
        if let Some(active) = quote {
            word.push(character);
            if character == '\\'
                && active == '"'
                && let Some(next) = chars.get(index + 1)
            {
                word.push(*next);
                index += 2;
                continue;
            }
            if character == active {
                quote = None;
            }
            index += 1;
            continue;
        }

        if matches!(character, '\'' | '"') {
            quote = Some(character);
            word.push(character);
            index += 1;
            continue;
        }
        if character == '\\' {
            let Some(next) = chars.get(index + 1).copied() else {
                return super::heredoc::restore_heredocs(
                    vec![command.to_string()],
                    &extraction.heredocs,
                );
            };
            if matches!(next, '(' | ')') {
                word.push('\\');
            }
            word.push(next);
            index += 2;
            continue;
        }
        if character.is_whitespace() && character != '\n' {
            flush_word(&mut word, &mut command_words);
            index += 1;
            continue;
        }
        if character == '#' && word.is_empty() && (index == 0 || chars[index - 1].is_whitespace()) {
            flush_command(&mut command_words, &mut parts);
            let mut comment = String::from("#");
            index += 1;
            while index < chars.len() && chars[index] != '\n' {
                comment.push(chars[index]);
                index += 1;
            }
            parts.push(comment);
            if index < chars.len() {
                index += 1;
            }
            continue;
        }

        let (operator, consumed) = match character {
            '\n' => (Some("\n"), 1),
            '(' => (Some("("), 1),
            ')' => (Some(")"), 1),
            ';' if chars.get(index + 1) == Some(&';') => (Some(";;"), 2),
            ';' => (Some(";"), 1),
            '|' if chars.get(index + 1) == Some(&'|') => (Some("||"), 2),
            '|' if chars.get(index + 1) == Some(&'&') => (Some("|&"), 2),
            '|' => (Some("|"), 1),
            '&' if chars.get(index + 1) == Some(&'&') => (Some("&&"), 2),
            '&' => (Some("&"), 1),
            '>' if chars.get(index + 1) == Some(&'>') => (Some(">>"), 2),
            '>' if chars.get(index + 1) == Some(&'&') => (Some(">&"), 2),
            '>' => (Some(">"), 1),
            '<' => (Some("<"), 1),
            _ => (None, 1),
        };
        if let Some(operator) = operator {
            flush_word(&mut word, &mut command_words);
            flush_command(&mut command_words, &mut parts);
            if operator != "\n" {
                parts.push(operator.to_string());
            }
            index += consumed;
            continue;
        }
        word.push(character);
        index += 1;
    }

    if quote.is_some() {
        return super::heredoc::restore_heredocs(vec![command.to_string()], &extraction.heredocs);
    }
    flush_word(&mut word, &mut command_words);
    flush_command(&mut command_words, &mut parts);
    super::heredoc::restore_heredocs(parts, &extraction.heredocs)
}

fn join_continuation_lines(command: &str) -> String {
    let bytes = command.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b'\\' {
            index += 1;
        }
        if bytes.get(index) == Some(&b'\n') && (index - start) % 2 == 1 {
            output.extend_from_slice(&bytes[start..index - 1]);
            index += 1;
        } else {
            output.extend_from_slice(&bytes[start..index]);
        }
    }
    String::from_utf8(output).unwrap_or_else(|_| command.to_string())
}

/// Maps to CC `filterControlOperators(...)`.
pub fn filter_control_operators(commands_and_operators: Vec<String>) -> Vec<String> {
    commands_and_operators
        .into_iter()
        .filter(|part| !ALL_SUPPORTED_CONTROL_OPERATORS.contains(&part.as_str()))
        .collect()
}

fn is_static_redirect_target(target: &str) -> bool {
    !target.is_empty()
        && !target.chars().any(char::is_whitespace)
        && !target.contains(['\'', '"', '$', '`', '*', '?', '[', '{', '~', '(', '<'])
        && !target.starts_with(['#', '!', '=', '&'])
}

/// Maps to CC `splitCommand_DEPRECATED(command)` for the legacy consumers that
/// remain active when the internal tree-sitter feature is unavailable.
pub fn split_command_deprecated(command: &str) -> Vec<String> {
    const FILE_DESCRIPTORS: &[&str] = &["0", "1", "2"];
    let mut parts = split_command_with_operators(command)
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();
    for index in 0..parts.len() {
        let Some(operator) = parts[index].as_deref() else {
            continue;
        };
        if !matches!(operator, ">&" | ">" | ">>") {
            continue;
        }
        let previous = index
            .checked_sub(1)
            .and_then(|previous| parts[previous].as_deref())
            .map(str::trim)
            .map(ToOwned::to_owned);
        let Some(next) = parts.get(index + 1).and_then(Option::as_deref) else {
            continue;
        };
        let after_next = parts.get(index + 2).and_then(Option::as_deref);
        let mut effective_next = next;
        if matches!(operator, ">" | ">>")
            && next.len() >= 3
            && next.as_bytes()[next.len() - 2] == b' '
            && FILE_DESCRIPTORS.contains(&&next[next.len() - 1..])
            && after_next.is_some_and(|part| matches!(part, ">" | ">>" | ">&"))
        {
            effective_next = &next[..next.len() - 2];
        }

        let mut strip_third = false;
        let should_strip = if operator == ">&" && FILE_DESCRIPTORS.contains(&next) {
            true
        } else if operator == ">"
            && next == "&"
            && after_next.is_some_and(|value| FILE_DESCRIPTORS.contains(&value))
        {
            strip_third = true;
            true
        } else if operator == ">"
            && next
                .strip_prefix('&')
                .is_some_and(|value| FILE_DESCRIPTORS.contains(&value))
        {
            true
        } else {
            matches!(operator, ">" | ">>") && is_static_redirect_target(effective_next)
        };
        if !should_strip {
            continue;
        }

        if let (Some(previous_index), Some(previous)) = (index.checked_sub(1), previous)
            && previous.len() >= 3
            && previous.as_bytes()[previous.len() - 2] == b' '
            && FILE_DESCRIPTORS.contains(&&previous[previous.len() - 1..])
        {
            parts[previous_index] = Some(previous[..previous.len() - 2].to_string());
        }
        parts[index] = None;
        parts[index + 1] = None;
        if strip_third {
            parts[index + 2] = None;
        }
    }

    filter_control_operators(
        parts
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect(),
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractOutputRedirectionsResult {
    pub command_without_redirections: String,
    pub redirections: Vec<super::parsed_command::OutputRedirection>,
    pub has_dangerous_redirection: bool,
}

fn entry_string(entry: Option<&ParseEntry>) -> Option<&str> {
    match entry {
        Some(ParseEntry::String(value)) => Some(value),
        _ => None,
    }
}

fn entry_operator(entry: Option<&ParseEntry>, operator: &str) -> bool {
    matches!(entry, Some(ParseEntry::Operator(value)) if value == operator)
}

fn is_file_descriptor(entry: Option<&ParseEntry>) -> bool {
    entry_string(entry).is_some_and(|value| {
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    })
}

fn is_simple_target(entry: Option<&ParseEntry>) -> Option<&str> {
    let target = entry_string(entry)?;
    (!target.is_empty()
        && !target.starts_with(['!', '=', '~'])
        && !target.contains(['$', '`', '*', '?', '[', '{']))
    .then_some(target)
}

fn has_dangerous_expansion(entry: Option<&ParseEntry>) -> bool {
    match entry {
        Some(ParseEntry::Glob(_)) => true,
        Some(ParseEntry::String(target)) if !target.is_empty() => {
            target.contains(['$', '%', '`', '*', '?', '[', '{'])
                || target.starts_with(['!', '=', '~'])
        }
        _ => false,
    }
}

fn push_redirection(
    redirections: &mut Vec<super::parsed_command::OutputRedirection>,
    target: &str,
    operator: &str,
) {
    redirections.push(super::parsed_command::OutputRedirection {
        target: target.to_string(),
        operator: operator.to_string(),
    });
}

fn handle_file_descriptor_redirection(
    fd: &str,
    operator: &str,
    target: Option<&ParseEntry>,
    redirections: &mut Vec<super::parsed_command::OutputRedirection>,
    kept: &mut Vec<ParseEntry>,
    skip_count: usize,
) -> (usize, bool) {
    let is_stdout = fd == "1";
    let simple_target = is_simple_target(target);
    let is_file_target = simple_target
        .is_some_and(|target| !target.chars().all(|character| character.is_ascii_digit()));
    let is_fd_target = is_file_descriptor(target);
    let _ = kept.pop();

    if !is_fd_target && has_dangerous_expansion(target) {
        return (0, true);
    }
    if is_file_target {
        let target = simple_target.expect("file target checked");
        push_redirection(redirections, target, operator);
        if !is_stdout {
            kept.push(ParseEntry::String(format!("{fd}{operator}")));
            kept.push(ParseEntry::String(target.to_string()));
        }
        return (skip_count, false);
    }
    if !is_stdout {
        kept.push(ParseEntry::String(format!("{fd}{operator}")));
        if let Some(ParseEntry::String(target)) = target {
            kept.push(ParseEntry::String(target.clone()));
            return (1, false);
        }
    }
    (0, false)
}

fn non_history_bang_target(value: &str) -> Option<&str> {
    let target = value.strip_prefix('!')?;
    let first = target.chars().next()?;
    (!matches!(first, '!' | '-' | '?') && !first.is_ascii_digit()).then_some(target)
}

fn handle_redirection(
    parts: &[ParseEntry],
    index: usize,
    redirections: &mut Vec<super::parsed_command::OutputRedirection>,
    kept: &mut Vec<ParseEntry>,
) -> (usize, bool) {
    let part = parts.get(index);
    let previous = index.checked_sub(1).and_then(|index| parts.get(index));
    let next = parts.get(index + 1);
    let next_next = parts.get(index + 2);
    let next_next_next = parts.get(index + 3);
    let operator = match part {
        Some(ParseEntry::Operator(operator)) if matches!(operator.as_str(), ">" | ">>") => {
            operator.as_str()
        }
        _ => "",
    };

    if !operator.is_empty() {
        if is_file_descriptor(previous) {
            let fd = entry_string(previous).expect("descriptor checked");
            if entry_string(next) == Some("!") && is_simple_target(next_next).is_some() {
                return handle_file_descriptor_redirection(
                    fd,
                    operator,
                    next_next,
                    redirections,
                    kept,
                    2,
                );
            }
            if entry_string(next) == Some("!") && has_dangerous_expansion(next_next) {
                return (0, true);
            }
            if entry_operator(next, "|") && is_simple_target(next_next).is_some() {
                return handle_file_descriptor_redirection(
                    fd,
                    operator,
                    next_next,
                    redirections,
                    kept,
                    2,
                );
            }
            if entry_operator(next, "|") && has_dangerous_expansion(next_next) {
                return (0, true);
            }
            if let Some(target) = entry_string(next).and_then(non_history_bang_target) {
                let target_entry = ParseEntry::String(target.to_string());
                if has_dangerous_expansion(Some(&target_entry)) {
                    return (0, true);
                }
                return handle_file_descriptor_redirection(
                    fd,
                    operator,
                    Some(&target_entry),
                    redirections,
                    kept,
                    1,
                );
            }
            return handle_file_descriptor_redirection(fd, operator, next, redirections, kept, 1);
        }

        if entry_operator(next, "|") {
            if let Some(target) = is_simple_target(next_next) {
                push_redirection(redirections, target, operator);
                return (2, false);
            }
            if has_dangerous_expansion(next_next) {
                return (0, true);
            }
        }
        if entry_string(next) == Some("!") {
            if let Some(target) = is_simple_target(next_next) {
                push_redirection(redirections, target, operator);
                return (2, false);
            }
            if has_dangerous_expansion(next_next) {
                return (0, true);
            }
        }
        if let Some(target) = entry_string(next).and_then(non_history_bang_target) {
            let target_entry = ParseEntry::String(target.to_string());
            if has_dangerous_expansion(Some(&target_entry)) {
                return (0, true);
            }
            push_redirection(redirections, target, operator);
            return (1, false);
        }
        if entry_operator(next, "&") {
            if entry_string(next_next) == Some("!") {
                if let Some(target) = is_simple_target(next_next_next) {
                    push_redirection(redirections, target, operator);
                    return (3, false);
                }
                if has_dangerous_expansion(next_next_next) {
                    return (0, true);
                }
            }
            if entry_operator(next_next, "|") {
                if let Some(target) = is_simple_target(next_next_next) {
                    push_redirection(redirections, target, operator);
                    return (3, false);
                }
                if has_dangerous_expansion(next_next_next) {
                    return (0, true);
                }
            }
            if let Some(target) = is_simple_target(next_next) {
                push_redirection(redirections, target, operator);
                return (2, false);
            }
            if has_dangerous_expansion(next_next) {
                return (0, true);
            }
        }
        if let Some(target) = is_simple_target(next) {
            push_redirection(redirections, target, operator);
            return (1, false);
        }
        if has_dangerous_expansion(next) {
            return (0, true);
        }
    }

    if entry_operator(part, ">&") {
        if is_file_descriptor(previous) && is_file_descriptor(next) {
            return (0, false);
        }
        if entry_operator(next, "|") {
            if let Some(target) = is_simple_target(next_next) {
                push_redirection(redirections, target, ">");
                return (2, false);
            }
            if has_dangerous_expansion(next_next) {
                return (0, true);
            }
        }
        if entry_string(next) == Some("!") {
            if let Some(target) = is_simple_target(next_next) {
                push_redirection(redirections, target, ">");
                return (2, false);
            }
            if has_dangerous_expansion(next_next) {
                return (0, true);
            }
        }
        if let Some(target) = is_simple_target(next).filter(|_| !is_file_descriptor(next)) {
            push_redirection(redirections, target, ">");
            return (1, false);
        }
        if !is_file_descriptor(next) && has_dangerous_expansion(next) {
            return (0, true);
        }
    }
    (0, false)
}

fn detect_command_substitution(previous: Option<&ParseEntry>) -> bool {
    entry_string(previous).is_some_and(|value| value == "$" || value.ends_with('$'))
}

fn needs_quoting(value: &str) -> bool {
    if value.len() >= 2
        && value.ends_with('>')
        && value
            .trim_end_matches('>')
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return false;
    }
    value
        .chars()
        .any(|character| character.is_whitespace() || character == '\u{feff}')
        || (value.chars().count() == 1 && "><|&;()".contains(value))
}

fn add_token(result: &mut String, token: &str, no_space: bool) {
    if !result.is_empty() && !no_space {
        result.push(' ');
    }
    result.push_str(token);
}

fn reconstruct_command(kept: &[ParseEntry], original_command: &str) -> String {
    if kept.is_empty() {
        return original_command.to_string();
    }
    let mut result = String::new();
    let mut command_substitution_depth = 0usize;
    let mut in_process_substitution = false;
    let mut index = 0usize;
    while index < kept.len() {
        let previous = index.checked_sub(1).and_then(|index| kept.get(index));
        let next = kept.get(index + 1);
        match &kept[index] {
            ParseEntry::String(value) => {
                let rendered = if value.contains(['|', '&', ';']) {
                    format!("\"{value}\"")
                } else if needs_quoting(value) {
                    super::shell_quote::quote(&[value])
                } else {
                    value.clone()
                };
                let no_space = result.ends_with('(')
                    || entry_string(previous) == Some("$")
                    || entry_operator(previous, ")");
                if result.ends_with("<(") {
                    result.push(' ');
                    result.push_str(&rendered);
                } else {
                    add_token(&mut result, &rendered, no_space);
                }
            }
            ParseEntry::Glob(pattern) => add_token(&mut result, pattern, false),
            ParseEntry::Comment(_) => {}
            ParseEntry::Operator(operator) if operator == ">&" => {
                if is_file_descriptor(previous) && is_file_descriptor(next) {
                    let previous = entry_string(previous).expect("descriptor checked");
                    let next = entry_string(next).expect("descriptor checked");
                    if let Some(position) = result.rfind(previous) {
                        result.truncate(position);
                        result.push_str(previous);
                        result.push_str(operator);
                        result.push_str(next);
                    }
                    index += 1;
                }
            }
            ParseEntry::Operator(operator) if operator == "<" && entry_operator(next, "<") => {
                if let Some(delimiter) = entry_string(kept.get(index + 2)) {
                    add_token(&mut result, delimiter, false);
                    index += 2;
                }
            }
            ParseEntry::Operator(operator) if operator == "<<<" => {
                add_token(&mut result, operator, false)
            }
            ParseEntry::Operator(operator) if operator == "(" => {
                let is_command_substitution = detect_command_substitution(previous);
                if is_command_substitution || command_substitution_depth > 0 {
                    command_substitution_depth += 1;
                    if result.ends_with(' ') {
                        let _ = result.pop();
                    }
                    result.push('(');
                } else {
                    let no_space = result.ends_with("<(") || result.ends_with('(');
                    add_token(&mut result, "(", no_space);
                }
            }
            ParseEntry::Operator(operator) if operator == ")" => {
                if in_process_substitution {
                    in_process_substitution = false;
                } else {
                    command_substitution_depth = command_substitution_depth.saturating_sub(1);
                }
                result.push(')');
            }
            ParseEntry::Operator(operator) if operator == "<(" => {
                in_process_substitution = true;
                add_token(&mut result, operator, false);
            }
            ParseEntry::Operator(operator)
                if matches!(
                    operator.as_str(),
                    "&&" | "||" | "|" | ";" | ">" | ">>" | "<"
                ) =>
            {
                add_token(&mut result, operator, false);
            }
            ParseEntry::Operator(_) => {}
        }
        index += 1;
    }
    let result = result.trim();
    if result.is_empty() {
        original_command.to_string()
    } else {
        result.to_string()
    }
}

/// Maps to CC `extractOutputRedirections(cmd)`.
pub fn extract_output_redirections(command: &str) -> ExtractOutputRedirectionsResult {
    let extraction = super::heredoc::extract_heredocs(command, false);
    let processed = join_continuation_lines(&extraction.processed_command);
    let parts = match super::shell_quote::try_parse_shell_command(&processed) {
        Ok(parts) => parts,
        Err(_) => {
            return ExtractOutputRedirectionsResult {
                command_without_redirections: command.to_string(),
                has_dangerous_redirection: true,
                ..ExtractOutputRedirectionsResult::default()
            };
        }
    };

    let mut redirected_subshells = std::collections::HashSet::new();
    let mut paren_stack = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if entry_operator(Some(part), "(") {
            let previous = index.checked_sub(1).and_then(|index| parts.get(index));
            let is_start = index == 0
                || matches!(previous, Some(ParseEntry::Operator(operator)) if matches!(operator.as_str(), "&&" | "||" | ";" | "|"));
            paren_stack.push((index, is_start));
        } else if entry_operator(Some(part), ")")
            && let Some((opening, is_start)) = paren_stack.pop()
        {
            let next = parts.get(index + 1);
            if is_start && (entry_operator(next, ">") || entry_operator(next, ">>")) {
                redirected_subshells.insert(opening);
                redirected_subshells.insert(index);
            }
        }
    }

    let mut kept = Vec::new();
    let mut redirections = Vec::new();
    let mut has_dangerous_redirection = false;
    let mut command_substitution_depth = 0usize;
    let mut index = 0usize;
    while index < parts.len() {
        let part = &parts[index];
        if (entry_operator(Some(part), "(") || entry_operator(Some(part), ")"))
            && redirected_subshells.contains(&index)
        {
            index += 1;
            continue;
        }
        let previous = index.checked_sub(1).and_then(|index| parts.get(index));
        if entry_operator(Some(part), "(")
            && entry_string(previous).is_some_and(|value| value.ends_with('$'))
        {
            command_substitution_depth += 1;
        } else if entry_operator(Some(part), ")") && command_substitution_depth > 0 {
            command_substitution_depth -= 1;
        }
        if command_substitution_depth == 0 {
            let (skip, dangerous) = handle_redirection(&parts, index, &mut redirections, &mut kept);
            has_dangerous_redirection |= dangerous;
            if skip > 0 {
                index += skip + 1;
                continue;
            }
        }
        kept.push(part.clone());
        index += 1;
    }

    let reconstructed = reconstruct_command(&kept, &processed);
    let command_without_redirections =
        super::heredoc::restore_heredocs(vec![reconstructed], &extraction.heredocs)
            .into_iter()
            .next()
            .unwrap_or_else(|| command.to_string());
    ExtractOutputRedirectionsResult {
        command_without_redirections,
        redirections,
        has_dangerous_redirection,
    }
}

/// Maps to CC `isHelpCommand(command)`.
pub fn is_help_command(command: &str) -> bool {
    let trimmed = command.trim();
    if !trimmed.ends_with("--help") || trimmed.contains(['\'', '"']) {
        return false;
    }
    let mut found_help = false;
    for token in trimmed.split_whitespace() {
        if token.starts_with('-') {
            if token != "--help" {
                return false;
            }
            found_help = true;
        } else if !token
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        {
            return false;
        }
    }
    found_help
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_preserves_quoted_operators_and_splits_top_level_compounds() {
        assert_eq!(
            split_command_with_operators("printf 'a|b' && git status; echo done"),
            ["printf 'a|b'", "&&", "git status", ";", "echo done"]
        );
        assert_eq!(
            split_command_deprecated("printf 'a|b' && git status; echo done"),
            ["printf 'a|b'", "git status", "echo done"]
        );
    }

    #[test]
    fn redirect_comment_subshell_and_unsupported_operator_matrix_matches_cc_2_1_88() {
        let fixtures: &[(&str, &[&str], &[&str])] = &[
            ("echo x > out", &["echo x", ">", "out"], &["echo x"]),
            (
                "echo x > /dev/null 2>&1",
                &["echo x", ">", "/dev/null 2", ">&", "1"],
                &["echo x"],
            ),
            (
                "echo x &> out",
                &["echo x", "&", ">", "out"],
                &["echo x", "&"],
            ),
            (
                "echo $(printf x; rm file)",
                &["echo $", "(", "printf x", ";", "rm file", ")"],
                &["echo $", "(", "printf x", "rm file", ")"],
            ),
            (
                "(cd a && pwd) || true",
                &["(", "cd a", "&&", "pwd", ")", "||", "true"],
                &["(", "cd a", "pwd", ")", "true"],
            ),
            (
                "echo hi # x && rm file",
                &["echo hi", "# x && rm file"],
                &["echo hi", "# x && rm file"],
            ),
            (
                "find . -exec echo {} \\; | head",
                &["find . -exec echo {} ;", "|", "head"],
                &["find . -exec echo {} ;", "head"],
            ),
            (
                "printf x |& cat",
                &["printf x", "|&", "cat"],
                &["printf x", "|&", "cat"],
            ),
            (
                "printf x & sleep 1",
                &["printf x", "&", "sleep 1"],
                &["printf x", "&", "sleep 1"],
            ),
            ("echo > &1", &["echo", ">", "&", "1"], &["echo"]),
        ];
        for (command, with_operators, deprecated) in fixtures {
            assert_eq!(
                split_command_with_operators(command),
                *with_operators,
                "with operators: {command:?}"
            );
            assert_eq!(
                split_command_deprecated(command),
                *deprecated,
                "deprecated: {command:?}"
            );
        }
    }

    #[test]
    fn heredoc_extraction_restoration_matches_cc_command_boundaries() {
        let command = "cat <<'EOF' && echo done\nbody; $(literal)\nEOF\nprintf tail";
        assert_eq!(
            split_command_with_operators(command),
            [
                "cat <<'EOF'\nbody; $(literal)\nEOF",
                "&&",
                "echo done",
                "printf tail",
            ]
        );
        assert_eq!(
            split_command_deprecated(command),
            [
                "cat <<'EOF'\nbody; $(literal)\nEOF",
                "echo done",
                "printf tail",
            ]
        );
    }

    #[test]
    fn malformed_quote_falls_back_to_one_joined_command() {
        assert_eq!(
            split_command_deprecated("echo 'x && rm file"),
            ["echo 'x && rm file"]
        );
        assert_eq!(split_command_deprecated("ec\\\nho ok"), ["echo ok"]);
    }

    #[test]
    fn output_redirection_matrix_matches_cc_2_1_88_legacy_owner() {
        let simple = extract_output_redirections("echo x 2>err >out");
        assert_eq!(simple.command_without_redirections, "echo x 2> err");
        assert_eq!(
            simple.redirections,
            [
                super::super::parsed_command::OutputRedirection {
                    target: "err".to_string(),
                    operator: ">".to_string(),
                },
                super::super::parsed_command::OutputRedirection {
                    target: "out".to_string(),
                    operator: ">".to_string(),
                },
            ]
        );
        assert!(!simple.has_dangerous_redirection);

        for (command, without, target) in [
            ("echo x >!out", "echo x", "out"),
            ("echo x >|out", "echo x", "out"),
            ("echo x >>&! out", "echo x", "out"),
            ("(echo x) > out", "echo x", "out"),
            (
                "echo $(printf x > inner) > outer",
                "echo $(printf x > inner)",
                "outer",
            ),
            ("echo x > \\\n/etc/passwd", "echo x", "/etc/passwd"),
        ] {
            let extracted = extract_output_redirections(command);
            assert_eq!(
                extracted.command_without_redirections, without,
                "{command:?}"
            );
            assert_eq!(extracted.redirections.len(), 1, "{command:?}");
            assert_eq!(extracted.redirections[0].target, target, "{command:?}");
            assert!(!extracted.has_dangerous_redirection, "{command:?}");
        }

        for command in [
            "echo x > '$HOME/literal'",
            "echo x > $HOME/out",
            "echo x > ~/out",
            "echo x > *.log",
            "echo x > `pwd`/out",
        ] {
            let extracted = extract_output_redirections(command);
            assert!(extracted.redirections.is_empty(), "{command:?}");
            assert!(extracted.has_dangerous_redirection, "{command:?}");
        }

        let heredoc = extract_output_redirections("cat <<'EOF' > out\n$HOME > hidden\nEOF");
        assert_eq!(
            heredoc.command_without_redirections,
            "cat <<'EOF'\n$HOME > hidden\nEOF"
        );
        assert_eq!(heredoc.redirections[0].target, "out");
    }

    #[test]
    fn help_command_matches_official_restrictions() {
        assert!(is_help_command("python module --help"));
        assert!(!is_help_command("python -m module --help"));
        assert!(!is_help_command("python 'module' --help"));
        assert!(!is_help_command("./python --help"));
    }
}
