//! Pipe-aware stdin redirect placement for Bash commands.
//!
//! Maps to: CC `utils/bash/bashPipeCommand.ts:10-280`.

fn single_quote_for_eval(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_with_eval_stdin_redirect(command: &str) -> String {
    format!("{} < /dev/null", single_quote_for_eval(command))
}

fn contains_control_structure(command: &str) -> bool {
    regex::Regex::new(r"\b(?:for|while|until|if|case|select)\s")
        .expect("valid Bash control-structure regex")
        .is_match(command)
}

fn join_continuation_lines(command: &str) -> String {
    let bytes = command.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
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

fn find_first_pipe_operator(parsed: &[super::shell_quote::ParseEntry]) -> Option<usize> {
    parsed.iter().position(
        |entry| matches!(entry, super::shell_quote::ParseEntry::Operator(operator) if operator == "|"),
    )
}

fn is_file_descriptor(value: &str) -> bool {
    matches!(value, "0" | "1" | "2")
}

fn is_environment_variable_assignment(value: &str) -> bool {
    let Some((name, _)) = value.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn build_command_parts(
    parsed: &[super::shell_quote::ParseEntry],
    start: usize,
    end: usize,
) -> Vec<String> {
    use super::shell_quote::ParseEntry;
    let mut parts = Vec::new();
    let mut seen_non_env_var = false;
    let mut index = start;
    while index < end {
        if let ParseEntry::String(descriptor) = &parsed[index]
            && is_file_descriptor(descriptor)
            && index + 2 < end
            && let (ParseEntry::Operator(operator), ParseEntry::String(target)) =
                (&parsed[index + 1], &parsed[index + 2])
        {
            if operator == ">&" && is_file_descriptor(target) {
                parts.push(format!("{descriptor}>&{target}"));
                index += 3;
                continue;
            }
            if operator == ">" && target == "/dev/null" {
                parts.push(format!("{descriptor}>/dev/null"));
                index += 3;
                continue;
            }
            if operator == ">" && target.strip_prefix('&').is_some_and(is_file_descriptor) {
                parts.push(format!("{descriptor}>{target}"));
                index += 3;
                continue;
            }
        }

        match &parsed[index] {
            ParseEntry::String(value) => {
                let is_env = !seen_non_env_var && is_environment_variable_assignment(value);
                if is_env {
                    let (name, value) = value.split_once('=').expect("assignment checked");
                    parts.push(format!("{name}={}", super::shell_quote::quote(&[value])));
                } else {
                    seen_non_env_var = true;
                    parts.push(super::shell_quote::quote(&[value]));
                }
            }
            ParseEntry::Glob(pattern) => parts.push(pattern.clone()),
            ParseEntry::Operator(operator) => {
                parts.push(operator.clone());
                if matches!(operator.as_str(), "&&" | "||" | ";") {
                    seen_non_env_var = false;
                }
            }
            ParseEntry::Comment(_) => {}
        }
        index += 1;
    }
    parts
}

/// Maps to CC `rearrangePipeCommand(command)`.
///
/// CC tokenizes and rebuilds the eligible simple-pipeline branch with
/// `shell-quote`. Rust preserves the same raw shell text and inserts the same
/// redirect immediately before the first unquoted pipe. All source fallback
/// conditions use CC's own `eval '<command>' < /dev/null` safe branch.
pub fn rearrange_pipe_command(command: &str) -> String {
    if command.contains('`')
        || command.contains("$(")
        || regex::Regex::new(r"\$[A-Za-z_{]")
            .expect("valid variable regex")
            .is_match(command)
        || contains_control_structure(command)
    {
        return quote_with_eval_stdin_redirect(command);
    }

    let joined = join_continuation_lines(command);
    if joined.contains('\n') || super::shell_quote::has_shell_quote_single_quote_bug(&joined) {
        return quote_with_eval_stdin_redirect(command);
    }
    let Ok(parsed) = super::shell_quote::try_parse_shell_command(&joined) else {
        return quote_with_eval_stdin_redirect(command);
    };
    if super::shell_quote::has_malformed_tokens(&joined, &parsed) {
        return quote_with_eval_stdin_redirect(command);
    }
    let Some(first_pipe_index) = find_first_pipe_operator(&parsed).filter(|index| *index > 0)
    else {
        return quote_with_eval_stdin_redirect(command);
    };
    let mut parts = build_command_parts(&parsed, 0, first_pipe_index);
    parts.push("< /dev/null".to_string());
    parts.extend(build_command_parts(&parsed, first_pipe_index, parsed.len()));
    single_quote_for_eval(&parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_pipe_places_redirect_after_first_command_like_official() {
        assert_eq!(
            rearrange_pipe_command("rg foo | wc -l"),
            "'rg foo < /dev/null | wc -l'"
        );
    }

    #[test]
    fn source_shaped_rebuild_matrix_matches_cc_2_1_88() {
        for (command, expected) in [
            ("FOO=bar env | cat", "'FOO=bar env < /dev/null | cat'"),
            ("echo *.rs | head", "'echo *.rs < /dev/null | head'"),
            ("cat file 2>&1 | head", "'cat file 2>&1 < /dev/null | head'"),
            (
                "cat file 2>/dev/null | head",
                "'cat file 2>/dev/null < /dev/null | head'",
            ),
            (
                "cat file 2> &1 | head",
                "'cat file 2 > & 1 < /dev/null | head'",
            ),
            (
                "echo \"a b\" | sed -n \"1p\"",
                "'echo '\"'\"'a b'\"'\"' < /dev/null | sed -n 1p'",
            ),
            ("echo café | head", "'echo café < /dev/null | head'"),
            ("echo 😀 | head", "'echo 😀 < /dev/null | head'"),
            ("echo $VALUE | cat", "'echo $VALUE | cat' < /dev/null"),
            ("echo x", "'echo x' < /dev/null"),
            (
                "echo x | cat && FOO=bar env | head",
                "'echo x < /dev/null | cat && FOO=bar env | head'",
            ),
            ("echo foo\\\nbar | cat", "'echo foobar < /dev/null | cat'"),
            (
                "echo \"unterminated | cat",
                "'echo \"unterminated | cat' < /dev/null",
            ),
            (
                "echo {\"hi\":\"hi;evil\"} | cat",
                "'echo \\{hi\\:hi\\;evil\\} < /dev/null | cat'",
            ),
            ("printf \"a|b\" | cat", "'printf a\\|b < /dev/null | cat'"),
        ] {
            assert_eq!(
                rearrange_pipe_command(command),
                expected,
                "CC 2.1.88 bashPipeCommand differential for {command:?}"
            );
        }
    }

    #[test]
    fn complex_pipe_uses_official_eval_redirect_fallback() {
        assert_eq!(
            rearrange_pipe_command("printf '%s' \"$(echo hi)\" | cat"),
            "'printf '\"'\"'%s'\"'\"' \"$(echo hi)\" | cat' < /dev/null"
        );
        assert_eq!(
            rearrange_pipe_command("echo $VALUE | cat"),
            "'echo $VALUE | cat' < /dev/null"
        );
    }
}
