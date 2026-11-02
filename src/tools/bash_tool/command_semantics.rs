//! Maps to: CC `tools/BashTool/commandSemantics.ts`.
//!
//! Exit-code interpretation only. Like the CC source, this is heuristic and is
//! not used for permission/security decisions.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInterpretation {
    pub is_error: bool,
    pub message: Option<String>,
}

/// Maps to CC `interpretCommandResult(command, exitCode, stdout, stderr)`.
pub fn interpret_command_result(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> CommandInterpretation {
    let _ = (stdout, stderr);
    match heuristically_extract_base_command(command).as_str() {
        "grep" | "rg" => CommandInterpretation {
            is_error: exit_code >= 2,
            message: (exit_code == 1).then(|| "No matches found".to_string()),
        },
        "find" => CommandInterpretation {
            is_error: exit_code >= 2,
            message: (exit_code == 1).then(|| "Some directories were inaccessible".to_string()),
        },
        "diff" => CommandInterpretation {
            is_error: exit_code >= 2,
            message: (exit_code == 1).then(|| "Files differ".to_string()),
        },
        "test" | "[" => CommandInterpretation {
            is_error: exit_code >= 2,
            message: (exit_code == 1).then(|| "Condition is false".to_string()),
        },
        _ => CommandInterpretation {
            is_error: exit_code != 0,
            message: (exit_code != 0).then(|| format!("Command failed with exit code {exit_code}")),
        },
    }
}

/// Maps to CC local `extractBaseCommand(command)`.
pub fn extract_base_command(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Maps to CC local `heuristicallyExtractBaseCommand(command)`.
pub fn heuristically_extract_base_command(command: &str) -> String {
    let segments = crate::utils::bash::commands::split_command_deprecated(command);
    let last_command = segments
        .last()
        .map(String::as_str)
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(command);
    extract_base_command(last_command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_specific_semantics_match_official_exit_code_meanings() {
        assert_eq!(
            interpret_command_result("grep needle file", 1, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("No matches found".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("rg needle", 2, "", "boom"),
            CommandInterpretation {
                is_error: true,
                message: None,
            }
        );
        assert_eq!(
            interpret_command_result("find /root", 1, "", "denied"),
            CommandInterpretation {
                is_error: false,
                message: Some("Some directories were inaccessible".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("diff a b", 1, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("Files differ".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("test -f missing", 1, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("Condition is false".to_string()),
            }
        );
    }

    #[test]
    fn default_semantics_and_last_segment_match_official_heuristic() {
        assert_eq!(
            heuristically_extract_base_command("echo hi | grep h"),
            "grep"
        );
        assert_eq!(
            interpret_command_result("cargo test", 101, "", ""),
            CommandInterpretation {
                is_error: true,
                message: Some("Command failed with exit code 101".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("cargo test", 0, "", ""),
            CommandInterpretation {
                is_error: false,
                message: None,
            }
        );
    }
}
