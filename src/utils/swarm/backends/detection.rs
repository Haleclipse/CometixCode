//! Swarm backend environment detection.
//! Maps to: CC `utils/swarm/backends/detection.ts`.

use std::process::Command;

use crate::utils::process_env::JsTruthy;
use crate::utils::swarm::constants::TMUX_COMMAND;

/// Maps to: CC `detection.ts:10` `ORIGINAL_USER_TMUX`, captured at module load
/// because Shell.ts may override `TMUX` later. `entrypoints/cli.rs` forces it
/// in the startup window.
pub(crate) static ORIGINAL_USER_TMUX: std::sync::LazyLock<Option<String>> =
    std::sync::LazyLock::new(|| crate::utils::process_env::var("TMUX"));

/// Maps to: CC `detection.ts:19` `ORIGINAL_TMUX_PANE`, the leader's pane,
/// captured at module load so a later pane switch does not move it.
pub(crate) static ORIGINAL_TMUX_PANE: std::sync::LazyLock<Option<String>> =
    std::sync::LazyLock::new(|| crate::utils::process_env::var("TMUX_PANE"));

/// Maps to: CC `ORIGINAL_USER_TMUX` consumer in `isInsideTmuxSync()`:
/// `!!ORIGINAL_USER_TMUX`.
pub fn is_inside_tmux_sync_from_env(original_user_tmux: Option<&str>) -> bool {
    original_user_tmux.truthy().is_some()
}

/// Maps to: CC `isInsideTmuxSync()`.
pub fn is_inside_tmux_sync() -> bool {
    is_inside_tmux_sync_from_env(ORIGINAL_USER_TMUX.as_deref())
}

/// Maps to: CC `isInsideTmux()`.
pub async fn is_inside_tmux() -> bool {
    is_inside_tmux_sync()
}

/// Maps to: CC `getLeaderPaneId()`: `ORIGINAL_TMUX_PANE || null`.
pub fn get_leader_pane_id() -> Option<String> {
    ORIGINAL_TMUX_PANE.clone().truthy()
}

/// Maps to: CC `isTmuxAvailable()`.
pub async fn is_tmux_available() -> bool {
    let mut command = Command::new(TMUX_COMMAND);
    // CC `execFileNoThrow` inherits process.env (execa default); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    command
        .arg("-V")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Maps to: CC `isInITerm2()`.
pub fn is_in_iterm2_from_env(
    term_program: Option<&str>,
    iterm_session_id: Option<&str>,
    detected_terminal: Option<&str>,
) -> bool {
    term_program == Some("iTerm.app")
        || iterm_session_id.truthy().is_some()
        || detected_terminal == Some("iTerm.app")
}

/// Maps to: CC `isInITerm2()`.
pub fn is_in_iterm2() -> bool {
    is_in_iterm2_from_env(
        crate::utils::process_env::var("TERM_PROGRAM").as_deref(),
        crate::utils::process_env::var("ITERM_SESSION_ID").as_deref(),
        crate::utils::env::get().terminal.as_deref(),
    )
}

/// Maps to: CC `IT2_COMMAND`.
pub const IT2_COMMAND: &str = "it2";

/// Maps to: CC `isIt2CliAvailable()`.
pub async fn is_it2_cli_available() -> bool {
    let mut command = Command::new(IT2_COMMAND);
    // CC `execFileNoThrow` inherits process.env (execa default); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    command
        .args(["session", "list"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Maps to: CC `resetDetectionCache()`.
///
/// Rust detection does not cache process-wide results yet, so this is a no-op
/// kept at the official boundary for tests/callers that mirror CC structure.
pub fn reset_detection_cache() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_detection_uses_original_tmux_env_only_like_official() {
        assert!(!is_inside_tmux_sync_from_env(None));
        assert!(!is_inside_tmux_sync_from_env(Some("")));
        assert!(is_inside_tmux_sync_from_env(Some(
            "/tmp/tmux-501/default,1,0"
        )));
    }

    #[test]
    fn iterm2_detection_checks_term_program_session_and_terminal() {
        assert!(is_in_iterm2_from_env(Some("iTerm.app"), None, None));
        assert!(is_in_iterm2_from_env(None, Some("w0t0p0"), None));
        assert!(is_in_iterm2_from_env(None, None, Some("iTerm.app")));
        assert!(!is_in_iterm2_from_env(Some("Apple_Terminal"), None, None));
    }
}
