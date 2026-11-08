//! FileSuggestion command execution.
//! Maps to: CC utils/hooks.ts:4675-4910 (executeFileSuggestionCommand).
//! Like statusline, this is a settings-configured command, not a hook event.

use super::exec::exec_command_hook;
use std::time::Duration;

const FILE_SUGGESTION_TIMEOUT_MS: u64 = 5_000;

/// Maps to: CC `executeFileSuggestionCommand()` (hooks.ts:4675-4910).
/// Runs the configured fileSuggestion command with a JSON payload containing
/// the current input prefix, and returns suggested file paths.
pub async fn execute_file_suggestion_command(
    command: &str,
    input_json: &str,
    base_env: Vec<(String, String)>,
) -> Option<Vec<String>> {
    if command.trim().is_empty() {
        return None;
    }

    // Maps to CC's per-call `shouldSkipHookDueToTrust()` gate at the start of
    // `executeFileSuggestionCommand` (hooks.ts:4681-4692). File suggestions
    // are not a HookEvent, so the generic event matcher is intentionally not
    // reused here. @nyan: CC's `shouldDisableAllHooksIncludingManaged()` gate
    // only reads managed settings, which nyan does not have.
    if super::security::should_skip_hook_due_to_trust(
        crate::utils::config::check_has_trust_dialog_accepted(),
        crate::bootstrap::state::get_is_non_interactive_session(),
    ) {
        return None;
    }

    let result = exec_command_hook(
        command,
        input_json,
        Duration::from_millis(FILE_SUGGESTION_TIMEOUT_MS),
        base_env,
        None,
        None,
        None,
    )
    .await;

    if result.aborted || result.status != 0 {
        return None;
    }

    let suggestions: Vec<String> = result
        .stdout
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if suggestions.is_empty() {
        None
    } else {
        Some(suggestions)
    }
}
