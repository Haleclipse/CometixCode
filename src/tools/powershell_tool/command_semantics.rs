//! Maps to: CC `tools/PowerShellTool/commandSemantics.ts`.
//!
//! Exit-code interpretation only — heuristic, and (per the source header)
//! never used for permission/security decisions.
//!
//! PowerShell-native cmdlets do NOT need exit-code semantics: `Select-String`,
//! `Compare-Object` and `Test-Path` all exit 0 and signal failure through
//! terminating errors instead. External executables invoked from PowerShell DO
//! set `$LASTEXITCODE`, and several use non-zero codes to convey information
//! rather than failure (`grep`/`rg`/`findstr` 1 = no match; `robocopy` 0-7 =
//! success). Without this module a `robocopy` that copied files (exit 1)
//! surfaces as a tool error.
//!
//! This is the PowerShell-flavoured sibling of
//! `tools/bash_tool/command_semantics.rs`; the two tables differ (CC
//! deliberately omits `diff`, `fc`, `find`, `test` and `[` here — see
//! `commandSemantics.ts:50-60`) so they stay separate files, as in CC.

/// Maps to: CC `commandSemantics.ts:20-27` `CommandSemantic` return shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInterpretation {
    pub is_error: bool,
    pub message: Option<String>,
}

/// Maps to: CC `commandSemantics.ts:32-36` `DEFAULT_SEMANTIC`.
fn default_semantic(exit_code: i32) -> CommandInterpretation {
    CommandInterpretation {
        is_error: exit_code != 0,
        message: (exit_code != 0).then(|| format!("Command failed with exit code {exit_code}")),
    }
}

/// Maps to: CC `commandSemantics.ts:41-44` `GREP_SEMANTIC`.
fn grep_semantic(exit_code: i32) -> CommandInterpretation {
    CommandInterpretation {
        is_error: exit_code >= 2,
        message: (exit_code == 1).then(|| "No matches found".to_string()),
    }
}

/// Maps to: CC `commandSemantics.ts:80-93` — the `robocopy` bitfield entry.
fn robocopy_semantic(exit_code: i32) -> CommandInterpretation {
    let message = if exit_code == 0 {
        Some("No files copied (already in sync)".to_string())
    } else if (1..8).contains(&exit_code) {
        // JS `exitCode & 1 ? ... : ...` — bit 0 means "files were copied".
        Some(if exit_code & 1 != 0 {
            "Files copied successfully".to_string()
        } else {
            "Robocopy completed (no errors)".to_string()
        })
    } else {
        None
    };
    CommandInterpretation {
        is_error: exit_code >= 8,
        message,
    }
}

/// Maps to: CC `commandSemantics.ts:100-111` `extractBaseCommand`.
///
/// Strips the PowerShell call operators (`&` / `.` followed by whitespace),
/// surrounding quotes, any directory prefix, and a trailing `.exe`.
pub fn extract_base_command(segment: &str) -> String {
    let stripped = {
        let trimmed = segment.trim_start();
        // JS `/^[&.]\s+/` — an operator char followed by at least one space.
        match trimmed.strip_prefix(['&', '.']) {
            Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
            _ => segment.trim(),
        }
    };
    let first_token = stripped.split_whitespace().next().unwrap_or_default();
    // JS `/^["']|["']$/g` strips at most one leading and one trailing quote.
    let unquoted = first_token.strip_prefix(['"', '\'']).unwrap_or(first_token);
    let unquoted = unquoted.strip_suffix(['"', '\'']).unwrap_or(unquoted);
    // JS `.split(/[\\/]/).pop() || unquoted` — `pop()` yields the empty string
    // when the token ends in a separator, and the `||` then falls back to the
    // whole token.
    let basename = unquoted
        .rsplit(['\\', '/'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(unquoted);
    let lowered = basename.to_lowercase();
    lowered.strip_suffix(".exe").unwrap_or(&lowered).to_string()
}

/// Maps to: CC `commandSemantics.ts:121-125` `heuristicallyExtractBaseCommand`.
///
/// The LAST pipeline segment determines the exit code. The split is a
/// deliberate heuristic (see the source note: false negatives just fall back
/// to the default semantic).
pub fn heuristically_extract_base_command(command: &str) -> String {
    let last = command
        .split([';', '|'])
        .rfind(|segment| !segment.trim().is_empty())
        .unwrap_or(command);
    extract_base_command(last)
}

/// Maps to: CC `commandSemantics.ts:130-142` `interpretCommandResult`.
pub fn interpret_command_result(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> CommandInterpretation {
    // CC threads stdout/stderr into every `CommandSemantic`; none of the
    // ported entries reads them, exactly as in the source.
    let _ = (stdout, stderr);
    match heuristically_extract_base_command(command).as_str() {
        // CC `COMMAND_SEMANTICS` (:62-94).
        "grep" | "rg" | "findstr" => grep_semantic(exit_code),
        "robocopy" => robocopy_semantic(exit_code),
        _ => default_semantic(exit_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_grep_family_exit_codes_match_official_semantics() {
        // CC `commandSemantics.ts:41-44` + the findstr entry at :69.
        for command in ["grep needle file", "rg needle", "findstr needle file"] {
            assert_eq!(
                interpret_command_result(command, 1, "", ""),
                CommandInterpretation {
                    is_error: false,
                    message: Some("No matches found".to_string()),
                },
                "{command} exit 1 is no-match, not failure"
            );
            assert_eq!(
                interpret_command_result(command, 2, "", "boom"),
                CommandInterpretation {
                    is_error: true,
                    message: None,
                }
            );
        }
    }

    #[test]
    fn powershell_robocopy_bitfield_matches_official_semantics() {
        // CC `commandSemantics.ts:80-93` — 0-7 success, 8+ failure.
        assert_eq!(
            interpret_command_result("robocopy src dst", 0, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("No files copied (already in sync)".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("robocopy src dst", 1, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("Files copied successfully".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("robocopy src dst", 2, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("Robocopy completed (no errors)".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("robocopy src dst", 3, "", ""),
            CommandInterpretation {
                is_error: false,
                message: Some("Files copied successfully".to_string()),
            }
        );
        assert_eq!(
            interpret_command_result("robocopy src dst", 8, "", ""),
            CommandInterpretation {
                is_error: true,
                message: None,
            }
        );
    }

    #[test]
    fn powershell_native_cmdlets_and_ambiguous_names_use_default_semantics() {
        // CC deliberately omits diff/fc/find/test/`[` and the native cmdlets
        // (`commandSemantics.ts:50-60`), so they must NOT pick up the Bash
        // table's meanings.
        for command in [
            "diff a b",
            "fc a b",
            "find .",
            "Select-String -Pattern x",
            "Compare-Object $a $b",
            "Test-Path x",
        ] {
            assert_eq!(
                interpret_command_result(command, 1, "", ""),
                CommandInterpretation {
                    is_error: true,
                    message: Some("Command failed with exit code 1".to_string()),
                },
                "{command} must fall through to DEFAULT_SEMANTIC"
            );
        }
    }

    #[test]
    fn base_command_extraction_matches_official_stripping_rules() {
        // CC `extractBaseCommand` (:100-111): call operator, quotes, path, .exe.
        assert_eq!(extract_base_command("& \"C:\\bin\\grep.exe\" x"), "grep");
        assert_eq!(extract_base_command(". .\\rg.exe needle"), "rg");
        assert_eq!(extract_base_command("  RoboCopy A B  "), "robocopy");
        // A bare `.` with no following space is a path segment, not the
        // call operator — JS `/^[&.]\s+/` requires the whitespace.
        assert_eq!(extract_base_command(".hidden-tool"), ".hidden-tool");
        // CC takes the LAST segment (:123) because it sets the exit code.
        assert_eq!(
            heuristically_extract_base_command("Get-Content f | findstr needle"),
            "findstr"
        );
        assert_eq!(
            heuristically_extract_base_command("Get-Date; robocopy a b"),
            "robocopy"
        );
        // Empty/all-separator input falls back to the whole command (:123).
        assert_eq!(heuristically_extract_base_command(";;"), ";;");
    }

    #[test]
    fn multibyte_commands_do_not_panic() {
        // Prefix slicing on bytes panics on multi-byte input; every step here
        // is char-safe.
        assert_eq!(
            interpret_command_result("Write-Output '日本語テスト'", 0, "", ""),
            CommandInterpretation {
                is_error: false,
                message: None,
            }
        );
        assert_eq!(extract_base_command("日本語"), "日本語");
    }
}
