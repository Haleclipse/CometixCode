//! Swarm backend environment detection.
//! Maps to: CC `utils/swarm/backends/detection.ts`.

use crate::utils::swarm::constants::TMUX_COMMAND;

/// Maps to: CC `ORIGINAL_USER_TMUX` consumer in `isInsideTmuxSync()`.
pub fn is_inside_tmux_sync_from_env(original_user_tmux: Option<&str>) -> bool {
    original_user_tmux.is_some_and(|value| !value.is_empty())
}

/// Maps to: CC `isInsideTmuxSync()`.
pub fn is_inside_tmux_sync() -> bool {
    is_inside_tmux_sync_from_env(crate::utils::process_env::var("TMUX").ok().as_deref())
}

/// Maps to: CC `isInsideTmux()`.
pub async fn is_inside_tmux() -> bool {
    is_inside_tmux_sync()
}

/// Maps to: CC `getLeaderPaneId()`.
pub fn get_leader_pane_id() -> Option<String> {
    crate::utils::process_env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Maps to: CC `isTmuxAvailable()`.
pub async fn is_tmux_available() -> bool {
    crate::utils::subprocess_env::command(TMUX_COMMAND)
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
        || iterm_session_id.is_some_and(|value| !value.is_empty())
        || detected_terminal == Some("iTerm.app")
}

/// Maps to: CC `isInITerm2()`.
pub fn is_in_iterm2() -> bool {
    is_in_iterm2_from_env(
        crate::utils::process_env::var("TERM_PROGRAM")
            .ok()
            .as_deref(),
        crate::utils::process_env::var("ITERM_SESSION_ID")
            .ok()
            .as_deref(),
        crate::utils::env::get().terminal.as_deref(),
    )
}

/// Maps to: CC `IT2_COMMAND`.
pub const IT2_COMMAND: &str = "it2";

/// Maps to: CC `isIt2CliAvailable()`.
pub async fn is_it2_cli_available() -> bool {
    crate::utils::subprocess_env::command(IT2_COMMAND)
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
