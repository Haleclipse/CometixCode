//! Maps to CC `tools/PowerShellTool/prompt.ts`.

use crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME;
use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
use crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME;
use crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
use crate::tools::grep_tool::prompt::GREP_TOOL_NAME;
use crate::tools::powershell_tool::tool_name::POWERSHELL_TOOL_NAME;
use crate::utils::shell::powershell_detection::{PowerShellEdition, get_powershell_edition};

pub fn get_default_timeout_ms() -> u64 {
    crate::tools::bash_tool::prompt::get_default_timeout_ms()
}

pub fn get_max_timeout_ms() -> u64 {
    crate::tools::bash_tool::prompt::get_max_timeout_ms()
}

/// Maps to: CC `prompt.ts:2` importing `getMaxOutputLength` from
/// `utils/shell/outputLimits.js`. Re-exported here (rather than reimplemented)
/// so the PowerShell prompt and the shell output cap cannot drift — the local
/// copy this replaced used a plain `parse::<u64>()` and so disagreed with
/// `validateBoundedIntEnvVar`'s JS `parseInt` prefix semantics on values like
/// `"30000abc"` and on the empty string.
pub fn get_max_output_length() -> usize {
    crate::utils::shell::output_limits::get_max_output_length()
}

fn get_background_usage_note() -> Option<&'static str> {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
            .as_deref(),
    ) {
        return None;
    }
    Some(
        "  - You can use the `run_in_background` parameter to run the command in the background. Only use this if you don't need the result immediately and are OK being notified when the command completes later. You do not need to check the output right away - you'll be notified when it finishes.",
    )
}

fn get_sleep_guidance() -> Option<&'static str> {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS")
            .as_deref(),
    ) {
        return None;
    }
    Some(
        "  - Avoid unnecessary `Start-Sleep` commands:\n    - Do not sleep between commands that can run immediately — just run them.\n    - If your command is long running and you would like to be notified when it finishes — simply run your command using `run_in_background`. There is no need to sleep in this case.\n    - Do not retry failing commands in a sleep loop — diagnose the root cause or consider an alternative approach.\n    - If waiting for a background task you started with `run_in_background`, you will be notified when it completes — do not poll.\n    - If you must poll an external process, use a check command rather than sleeping first.\n    - If you must sleep, keep the duration short (1-5 seconds) to avoid blocking the user.",
    )
}

/// Maps to: CC `prompt.ts:51-71` `getEditionSection(edition)`.
///
/// The model's training data covers both editions but it cannot tell which one
/// it is targeting, so it either emits pwsh-7 syntax on 5.1 (parser error) or
/// needlessly avoids `&&` on 7. `None` is CC's "detection not yet resolved or
/// PS not installed" branch (:66-70) and gives the conservative 5.1 guidance.
fn get_edition_section(edition: Option<PowerShellEdition>) -> &'static str {
    match edition {
        Some(PowerShellEdition::Desktop) => {
            "PowerShell edition: Windows PowerShell 5.1 (powershell.exe)\n   - Pipeline chain operators `&&` and `||` are NOT available — they cause a parser error. To run B only if A succeeds: `A; if ($?) { B }`. To chain unconditionally: `A; B`.\n   - Ternary (`?:`), null-coalescing (`??`), and null-conditional (`?.`) operators are NOT available. Use `if/else` and explicit `$null -eq` checks instead.\n   - Avoid `2>&1` on native executables. In 5.1, redirecting a native command's stderr inside PowerShell wraps each line in an ErrorRecord (NativeCommandError) and sets `$?` to `$false` even when the exe returned exit code 0. stderr is already captured for you — don't redirect it.\n   - Default file encoding is UTF-16 LE (with BOM). When writing files other tools will read, pass `-Encoding utf8` to `Out-File`/`Set-Content`.\n   - `ConvertFrom-Json` returns a PSCustomObject, not a hashtable. `-AsHashtable` is not available."
        }
        Some(PowerShellEdition::Core) => {
            "PowerShell edition: PowerShell 7+ (pwsh)\n   - Pipeline chain operators `&&` and `||` ARE available and work like bash. Prefer `cmd1 && cmd2` over `cmd1; cmd2` when cmd2 should only run if cmd1 succeeds.\n   - Ternary (`$cond ? $a : $b`), null-coalescing (`??`), and null-conditional (`?.`) operators are available.\n   - Default file encoding is UTF-8 without BOM."
        }
        None => {
            "PowerShell edition: unknown — assume Windows PowerShell 5.1 for compatibility\n   - Do NOT use `&&`, `||`, ternary `?:`, null-coalescing `??`, or null-conditional `?.`. These are PowerShell 7+ only and parser-error on 5.1.\n   - To chain commands conditionally: `A; if ($?) { B }`. Unconditionally: `A; B`."
        }
    }
}

/// Maps to CC `getPrompt()` (:73-145).
pub fn get_prompt() -> String {
    let background_note = get_background_usage_note()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    let sleep_guidance = get_sleep_guidance()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    format!(
        "Executes a given PowerShell command with optional timeout. Working directory persists between commands; shell state (variables, functions) does not.\n\nIMPORTANT: This tool is for terminal operations via PowerShell: git, npm, docker, and PS cmdlets. DO NOT use it for file operations (reading, writing, editing, searching, finding files) - use the specialized tools for this instead.\n\n{}\n\nBefore executing the command, please follow these steps:\n\n1. Directory Verification:\n   - If the command will create new directories or files, first use `Get-ChildItem` (or `ls`) to verify the parent directory exists and is the correct location\n\n2. Command Execution:\n   - Always quote file paths that contain spaces with double quotes\n   - Capture the output of the command.\n\nPowerShell Syntax Notes:\n   - Variables use $ prefix: $myVar = \"value\"\n   - Escape character is backtick (`), not backslash\n   - Use Verb-Noun cmdlet naming: Get-ChildItem, Set-Location, New-Item, Remove-Item\n   - Common aliases: ls (Get-ChildItem), cd (Set-Location), cat (Get-Content), rm (Remove-Item)\n   - Pipe operator | works similarly to bash but passes objects, not text\n   - Use Select-Object, Where-Object, ForEach-Object for filtering and transformation\n   - String interpolation: \"Hello $name\" or \"Hello $($obj.Property)\"\n   - Registry access uses PSDrive prefixes: `HKLM:\\SOFTWARE\\...`, `HKCU:\\...` — NOT raw `HKEY_LOCAL_MACHINE\\...`\n   - Environment variables: read with `$env:NAME`, set with `$env:NAME = \"value\"` (NOT `Set-Variable` or bash `export`)\n   - Call native exe with spaces in path via call operator: `& \"C:\\Program Files\\App\\app.exe\" arg1 arg2`\n\nInteractive and blocking commands (will hang — this tool runs with -NonInteractive):\n   - NEVER use `Read-Host`, `Get-Credential`, `Out-GridView`, `$Host.UI.PromptForChoice`, or `pause`\n   - Destructive cmdlets (`Remove-Item`, `Stop-Process`, `Clear-Content`, etc.) may prompt for confirmation. Add `-Confirm:$false` when you intend the action to proceed. Use `-Force` for read-only/hidden items.\n   - Never use `git rebase -i`, `git add -i`, or other commands that open an interactive editor\n\nPassing multiline strings (commit messages, file content) to native executables:\n   - Use a single-quoted here-string so PowerShell does not expand `$` or backticks inside. The closing `'@` MUST be at column 0 (no leading whitespace) on its own line — indenting it is a parse error:\n<example>\ngit commit -m @'\nCommit message here.\nSecond line with $literal dollar signs.\n'@\n</example>\n   - Use `@'...'@` (single-quoted, literal) not `@\"...\"@` (double-quoted, interpolated) unless you need variable expansion\n   - For arguments containing `-`, `@`, or other characters PowerShell parses as operators, use the stop-parsing token: `git log --% --format=%H`\n\nUsage notes:\n  - The command argument is required.\n  - You can specify an optional timeout in milliseconds (up to {}ms / {} minutes). If not specified, commands will timeout after {}ms ({} minutes).\n  - It is very helpful if you write a clear, concise description of what this command does.\n  - If the output exceeds {} characters, output will be truncated before being returned to you.\n{}  - Avoid using PowerShell to run commands that have dedicated tools, unless explicitly instructed:\n    - File search: Use {GLOB_TOOL_NAME} (NOT Get-ChildItem -Recurse)\n    - Content search: Use {GREP_TOOL_NAME} (NOT Select-String)\n    - Read files: Use {FILE_READ_TOOL_NAME} (NOT Get-Content)\n    - Edit files: Use {FILE_EDIT_TOOL_NAME}\n    - Write files: Use {FILE_WRITE_TOOL_NAME} (NOT Set-Content/Out-File)\n    - Communication: Output text directly (NOT Write-Output/Write-Host)\n  - When issuing multiple commands:\n    - If the commands are independent and can run in parallel, make multiple {POWERSHELL_TOOL_NAME} tool calls in a single message.\n    - If the commands depend on each other and must run sequentially, chain them in a single {POWERSHELL_TOOL_NAME} call (see edition-specific chaining syntax above).\n    - Use `;` only when you need to run commands sequentially but don't care if earlier commands fail.\n    - DO NOT use newlines to separate commands (newlines are ok in quoted strings and here-strings)\n  - Do NOT prefix commands with `cd` or `Set-Location` -- the working directory is already set to the correct project directory automatically.\n{}  - For git commands:\n    - Prefer to create a new commit rather than amending an existing commit.\n    - Before running destructive operations (e.g., git reset --hard, git push --force, git checkout --), consider whether there is a safer alternative that achieves the same goal. Only use destructive operations when they are truly the best approach.\n    - Never skip hooks (--no-verify) or bypass signing (--no-gpg-sign, -c commit.gpgsign=false) unless the user has explicitly asked for it. If a hook fails, investigate and fix the underlying issue.",
        get_edition_section(get_powershell_edition()),
        get_max_timeout_ms(),
        get_max_timeout_ms() / 60000,
        get_default_timeout_ms(),
        get_default_timeout_ms() / 60000,
        get_max_output_length(),
        background_note,
        sleep_guidance,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_carries_the_official_here_string_section() {
        // CC `prompt.ts:110-119`. The whole block was absent, so the model was
        // never told how to pass a multiline commit message to a native exe,
        // nor about the `--%` stop-parsing token.
        let prompt = get_prompt();
        assert!(prompt.contains(
            "Passing multiline strings (commit messages, file content) to native executables:"
        ));
        assert!(prompt.contains("The closing `'@` MUST be at column 0"));
        assert!(prompt.contains("<example>\ngit commit -m @'\n"));
        assert!(prompt.contains("'@\n</example>"));
        assert!(prompt.contains("use the stop-parsing token: `git log --% --format=%H`"));
        // The section sits between the interactive-commands block and
        // "Usage notes:" (:105-121).
        let interactive = prompt
            .find("Interactive and blocking commands")
            .expect("interactive section present");
        let here_strings = prompt
            .find("Passing multiline strings")
            .expect("here-string section present");
        let usage = prompt.find("Usage notes:").expect("usage notes present");
        assert!(interactive < here_strings && here_strings < usage);
    }

    #[test]
    fn edition_sections_match_official_branches() {
        // CC `prompt.ts:51-71`. The port used to hardcode the `None` branch, so
        // a pwsh-7 host was told NOT to use `&&`.
        let core = get_edition_section(Some(PowerShellEdition::Core));
        assert!(core.starts_with("PowerShell edition: PowerShell 7+ (pwsh)"));
        assert!(core.contains("`&&` and `||` ARE available"));
        assert!(core.contains("Default file encoding is UTF-8 without BOM."));

        let desktop = get_edition_section(Some(PowerShellEdition::Desktop));
        assert!(desktop.starts_with("PowerShell edition: Windows PowerShell 5.1 (powershell.exe)"));
        assert!(desktop.contains("`&&` and `||` are NOT available"));
        assert!(desktop.contains("`-AsHashtable` is not available."));

        let unknown = get_edition_section(None);
        assert!(unknown.starts_with("PowerShell edition: unknown"));

        // get_prompt() renders whichever branch detection resolves to.
        let prompt = get_prompt();
        assert!(prompt.contains(get_edition_section(get_powershell_edition())));
    }

    #[test]
    fn prompt_uses_the_shared_output_limit_owner() {
        // CC `prompt.ts:2` imports getMaxOutputLength; the local copy this
        // replaced diverged from validateBoundedIntEnvVar on prefix parses.
        assert_eq!(
            get_max_output_length(),
            crate::utils::shell::output_limits::get_max_output_length()
        );
        assert!(get_prompt().contains(&format!(
            "If the output exceeds {} characters",
            get_max_output_length()
        )));
    }

    #[test]
    fn prompt_names_the_dedicated_tools_from_their_owning_constants() {
        // CC `prompt.ts:11-15,128-132` interpolates the tool-name constants.
        let prompt = get_prompt();
        assert!(prompt.contains(&format!(
            "File search: Use {GLOB_TOOL_NAME} (NOT Get-ChildItem -Recurse)"
        )));
        assert!(prompt.contains(&format!(
            "Content search: Use {GREP_TOOL_NAME} (NOT Select-String)"
        )));
        assert!(prompt.contains(&format!(
            "Read files: Use {FILE_READ_TOOL_NAME} (NOT Get-Content)"
        )));
        assert!(prompt.contains(&format!("Edit files: Use {FILE_EDIT_TOOL_NAME}")));
        assert!(prompt.contains(&format!(
            "Write files: Use {FILE_WRITE_TOOL_NAME} (NOT Set-Content/Out-File)"
        )));
    }
}
