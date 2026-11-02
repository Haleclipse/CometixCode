//! Maps to: CC `utils/bash/shellQuote.ts`.
//!
//! Owns parsing/quoting plus the legacy shell-quote differential guards
//! consumed by Bash permission checks.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseEntry {
    String(String),
    Operator(String),
    Glob(String),
    Comment(String),
}

// L1: shell-quote's ECMAScript /\s/ set (Rust includes NEL and omits BOM).
fn is_shell_whitespace(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}'
        | '\u{3000}' | '\u{feff}')
}

fn control_operator(chars: &[char], index: usize) -> Option<&'static str> {
    for operator in ["||", "&&", ";;", "|&", "<(", "<<<", ">>", ">&", "<&"] {
        let operator_chars = operator.chars().collect::<Vec<_>>();
        if chars.get(index..index + operator_chars.len()) == Some(operator_chars.as_slice()) {
            return Some(operator);
        }
    }
    chars.get(index).and_then(|character| match character {
        '&' => Some("&"),
        ';' => Some(";"),
        '(' => Some("("),
        ')' => Some(")"),
        '|' => Some("|"),
        '<' => Some("<"),
        '>' => Some(">"),
        _ => None,
    })
}

fn closing_quote(chars: &[char], start: usize, quote: char) -> Option<usize> {
    let mut index = start + 1;
    while index < chars.len() {
        if quote == '"' && chars[index] == '\\' {
            index += 2;
            continue;
        }
        if chars[index] == quote {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn parse_env_var(chars: &[char], dollar: usize) -> Result<(String, usize), String> {
    let mut index = dollar + 1;
    let Some(character) = chars.get(index).copied() else {
        return Ok(("$".to_string(), index));
    };
    let name = if character == '{' {
        index += 1;
        let start = index;
        while chars.get(index).is_some_and(|character| *character != '}') {
            index += 1;
        }
        if index == start || chars.get(index) != Some(&'}') {
            return Err("Bad substitution".to_string());
        }
        let name = chars[start..index].iter().collect::<String>();
        index += 1;
        name
    } else if "*@#?$!_-".contains(character) {
        index += 1;
        character.to_string()
    } else if character.is_ascii_alphanumeric() || character == '_' {
        let start = index;
        while chars
            .get(index)
            .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '_')
        {
            index += 1;
        }
        chars[start..index].iter().collect()
    } else {
        String::new()
    };
    Ok((format!("${name}"), index))
}

/// Maps to CC `tryParseShellCommand(command, env?)` using the same shell-quote
/// token model needed by legacy Bash security consumers.
pub fn try_parse_shell_command(command: &str) -> Result<Vec<ParseEntry>, String> {
    let chars = command.chars().collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut index = 0usize;
    let mut commented = false;

    while index < chars.len() && !commented {
        if is_shell_whitespace(chars[index]) {
            index += 1;
            continue;
        }
        if let Some(operator) = control_operator(&chars, index) {
            entries.push(ParseEntry::Operator(operator.to_string()));
            index += operator.chars().count();
            continue;
        }

        let mut output = String::new();
        let mut is_glob = false;
        let mut consumed_fragment = false;
        loop {
            if index >= chars.len()
                || is_shell_whitespace(chars[index])
                || control_operator(&chars, index).is_some()
            {
                break;
            }
            if matches!(chars[index], '\'' | '"') {
                let quote = chars[index];
                let Some(end) = closing_quote(&chars, index, quote) else {
                    // shell-quote's chunk regex skips an unmatched delimiter;
                    // text after it is parsed as a separate token.
                    index += 1;
                    break;
                };
                consumed_fragment = true;
                index += 1;
                while index < end {
                    let character = chars[index];
                    if quote == '"'
                        && character == '\\'
                        && let Some(next) = chars.get(index + 1).copied()
                    {
                        if matches!(next, '"' | '\\' | '$') {
                            output.push(next);
                        } else {
                            output.push('\\');
                            output.push(next);
                        }
                        index += 2;
                        continue;
                    }
                    if quote == '"' && character == '$' {
                        let (value, next) = parse_env_var(&chars, index)?;
                        output.push_str(&value);
                        index = next;
                        continue;
                    }
                    output.push(character);
                    index += 1;
                }
                index = end + 1;
                continue;
            }

            consumed_fragment = true;
            let character = chars[index];
            if character == '#' {
                let comment = chars[index + 1..].iter().collect::<String>();
                if !output.is_empty() {
                    entries.push(if is_glob {
                        ParseEntry::Glob(std::mem::take(&mut output))
                    } else {
                        ParseEntry::String(std::mem::take(&mut output))
                    });
                }
                entries.push(ParseEntry::Comment(comment));
                consumed_fragment = false;
                commented = true;
                index = chars.len();
                break;
            }
            if character == '\\' {
                if let Some(next) = chars.get(index + 1).copied() {
                    output.push(next);
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }
            if character == '$' {
                let (value, next) = parse_env_var(&chars, index)?;
                output.push_str(&value);
                index = next;
                continue;
            }
            is_glob |= matches!(character, '*' | '?');
            output.push(character);
            index += 1;
        }
        if !output.is_empty() || consumed_fragment {
            entries.push(if is_glob {
                ParseEntry::Glob(output)
            } else {
                ParseEntry::String(output)
            });
        }
    }
    Ok(entries)
}

/// Quote shell arguments and join them with spaces.
///
/// Maps to: CC `utils/bash/shellQuote.ts::quote`.
pub fn quote(args: &[&str]) -> String {
    args.iter()
        .map(|argument| quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Maps to CC `hasShellQuoteSingleQuoteBug(command)`.
pub fn has_shell_quote_single_quote_bug(command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut in_single = false;
    let mut in_double = false;
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character == '\\' && !in_single {
            index = (index + 2).min(chars.len());
            continue;
        }
        if character == '"' && !in_single {
            in_double = !in_double;
            index += 1;
            continue;
        }
        if character == '\'' && !in_double {
            in_single = !in_single;
            if !in_single {
                let mut backslashes = 0usize;
                let mut cursor = index;
                while cursor > 0 && chars[cursor - 1] == '\\' {
                    backslashes += 1;
                    cursor -= 1;
                }
                if backslashes % 2 == 1 || (backslashes > 0 && chars[index + 1..].contains(&'\'')) {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

/// Maps to CC `hasMalformedTokens(command, parsed)`.
pub fn has_malformed_tokens(command: &str, parsed: &[ParseEntry]) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut in_single = false;
    let mut in_double = false;
    let mut single_count = 0usize;
    let mut double_count = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];
        if character == '\\' && !in_single {
            index = (index + 2).min(chars.len());
            continue;
        }
        if character == '"' && !in_single {
            double_count += 1;
            in_double = !in_double;
        } else if character == '\'' && !in_double {
            single_count += 1;
            in_single = !in_single;
        }
        index += 1;
    }
    if !double_count.is_multiple_of(2) || !single_count.is_multiple_of(2) {
        return true;
    }

    parsed.iter().any(|entry| {
        let ParseEntry::String(token) = entry else {
            return false;
        };
        [('{', '}'), ('(', ')'), ('[', ']')]
            .into_iter()
            .any(|(open, close)| {
                token.chars().filter(|character| *character == open).count()
                    != token
                        .chars()
                        .filter(|character| *character == close)
                        .count()
            })
            || ['"', '\''].into_iter().any(|quote| {
                let chars = token.chars().collect::<Vec<_>>();
                chars
                    .iter()
                    .enumerate()
                    .filter(|(index, character)| {
                        **character == quote && (*index == 0 || chars.get(index - 1) != Some(&'\\'))
                    })
                    .count()
                    % 2
                    != 0
            })
    })
}

/// Source-shaped caller boundary used by Bash security: malformed tokens only
/// matter when shell-quote exposed a list separator.
pub fn has_malformed_syntax(command: &str) -> bool {
    let Ok(parsed) = try_parse_shell_command(command) else {
        return false;
    };
    let has_command_separator = parsed.iter().any(|entry| {
        matches!(entry, ParseEntry::Operator(operator) if matches!(operator.as_str(), ";" | "&&" | "||"))
    });
    has_command_separator && has_malformed_tokens(command, &parsed)
}

fn quote_argument(argument: &str) -> String {
    if argument.is_empty() {
        return "''".to_string();
    }
    let js_whitespace = is_shell_whitespace;
    if (argument.contains('"') || argument.contains('\\') || argument.chars().any(js_whitespace))
        && !argument.contains('\'')
    {
        return format!("'{argument}'");
    }
    if argument.contains('"') || argument.contains('\'') || argument.chars().any(js_whitespace) {
        let mut escaped = String::with_capacity(argument.len());
        for character in argument.chars() {
            if matches!(character, '"' | '\\' | '$' | '`' | '!') {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        return format!("\"{escaped}\"");
    }

    let punctuation = "#!\"$&'()*,:;<=>?@[\\]^`{|}";
    let chars = argument.chars().collect::<Vec<_>>();
    let mut escaped = String::with_capacity(argument.len());
    for (index, character) in chars.iter().copied().enumerate() {
        let drive_colon = character == ':'
            && index == 1
            && chars
                .first()
                .is_some_and(|first| first.is_ascii_alphabetic());
        if punctuation.contains(character) && !drive_colon {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_matches_shell_quote_shapes_used_by_resume_commands() {
        assert_eq!(quote(&["/repo/path"]), "/repo/path");
        assert_eq!(quote(&["/other project"]), "'/other project'");
        assert_eq!(quote(&["a'b"]), "\"a'b\"");
        assert_eq!(quote(&["a'\"b"]), "\"a'\\\"b\"");
        assert_eq!(quote(&["$HOME"]), "\\$HOME");
        assert_eq!(quote(&["*"]), "\\*");
        assert_eq!(quote(&[""]), "''");
        assert_eq!(quote(&["a b", "c"]), "'a b' c");
    }

    #[test]
    fn legacy_shell_quote_differential_guards_match_official_attacks() {
        assert!(has_shell_quote_single_quote_bug(
            r"git ls-remote 'safe\\' '--upload-pack=evil' 'repo'"
        ));
        assert!(!has_shell_quote_single_quote_bug(r"printf 'C:\\path'"));
        assert!(!has_malformed_syntax(r#"echo "unterminated"#));
        assert!(has_malformed_syntax(r#"echo "hi;evil | cat"#));
        assert!(has_malformed_syntax("echo {foo; cat"));
        assert!(!has_malformed_syntax("echo foo(bar; cat"));
        assert!(!has_malformed_syntax(r#"echo {"hi":"hi;evil"}"#));
        assert!(!has_malformed_syntax(r#"echo '{"hi":"safe"}'"#));
    }
    #[test]
    fn shell_whitespace_matches_official_bun_nel_and_bom() {
        // Actual source shellQuote.ts oracle captures parse and quote outputs.
        assert_eq!(
            try_parse_shell_command("a\u{0085}b").unwrap(),
            vec![ParseEntry::String("a\u{0085}b".into())]
        );
        assert_eq!(
            try_parse_shell_command("a\u{feff}b").unwrap(),
            vec![
                ParseEntry::String("a".into()),
                ParseEntry::String("b".into())
            ]
        );
        assert_eq!(
            try_parse_shell_command("a\u{00a0}b").unwrap(),
            vec![
                ParseEntry::String("a".into()),
                ParseEntry::String("b".into())
            ]
        );
        assert_eq!(quote(&["a\u{0085}b"]), "a\u{0085}b");
        assert_eq!(quote(&["a\u{feff}b"]), "'a\u{feff}b'");
    }
}
