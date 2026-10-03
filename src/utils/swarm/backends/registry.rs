//! Swarm backend registry and mode resolution.
//! Maps to: CC `utils/swarm/backends/registry.ts`.

use std::sync::{LazyLock, Mutex};

use super::detection;
use super::pane_backend_executor::{PaneBackendExecutor, create_pane_backend_executor};
use super::teammate_mode_snapshot::{TeammateMode, get_teammate_mode_from_snapshot};
use super::types::{BackendDetectionResult, PaneBackendType};
#[cfg(test)]
use crate::utils::test_env::TestStateLock;

#[derive(Default)]
struct BackendRegistryState {
    in_process_fallback: bool,
    /// CC caches only the SUCCESS shape (`cachedDetectionResult`, `:31`) and
    /// re-runs detection after a throw. The outer `Option` is "not detected
    /// yet"; caching the `Err` too keeps a single detection per process, which
    /// is what CC's comment at `:24-25` promises ("fixed for the lifetime of
    /// the process") and what `resetBackendDetection` exists to undo.
    cached_detection: Option<Result<BackendDetectionResult, String>>,
    cached_pane_executor: Option<PaneBackendExecutor>,
}

static BACKEND_REGISTRY: LazyLock<Mutex<BackendRegistryState>> =
    LazyLock::new(|| Mutex::new(BackendRegistryState::default()));

#[cfg(test)]
pub(crate) static TEST_BACKEND_REGISTRY_LOCK: LazyLock<TestStateLock> =
    LazyLock::new(TestStateLock::new);

/// Maps to: CC `markInProcessFallback()`.
pub fn mark_in_process_fallback() {
    BACKEND_REGISTRY.lock().unwrap().in_process_fallback = true;
}

/// Maps to: CC `resetBackendDetection()`.
pub fn reset_backend_detection() {
    *BACKEND_REGISTRY.lock().unwrap() = BackendRegistryState::default();
    detection::reset_detection_cache();
}

/// Maps to: CC `isInProcessEnabled()` decision logic.
pub fn is_in_process_enabled_from_parts(
    is_non_interactive_session: bool,
    teammate_mode: TeammateMode,
    in_process_fallback: bool,
    inside_tmux: bool,
    in_iterm2: bool,
) -> bool {
    if is_non_interactive_session || teammate_mode == TeammateMode::InProcess {
        return true;
    }
    if teammate_mode == TeammateMode::Tmux {
        return false;
    }
    if in_process_fallback {
        return true;
    }
    !inside_tmux && !in_iterm2
}

/// Maps to: CC `isInProcessEnabled()`.
pub fn is_in_process_enabled() -> bool {
    is_in_process_enabled_from_parts(
        crate::bootstrap::state::get_is_non_interactive_session(),
        get_teammate_mode_from_snapshot(),
        BACKEND_REGISTRY.lock().unwrap().in_process_fallback,
        detection::is_inside_tmux_sync(),
        detection::is_in_iterm2(),
    )
}

/// Maps to: CC `registry.ts:136-254#detectAndGetBackend`.
///
/// CC THROWS on the two no-backend branches (`:228-230` iTerm2 with neither it2
/// nor tmux, `:253` no tmux at all) and the callers distinguish them: the auto
/// fallback in `spawnMultiAgent.ts:1053-1069` rethrows the ORIGINAL error when
/// the user pinned `teammateMode: 'tmux'`. This used to return `Option`, which
/// erased both the distinction and the message — every failure surfaced as
/// `getTmuxInstallInstructions()` — and it also invented a fourth
/// `Some(ITerm2 { needs_it2_setup: true })` result for CC's `:228` throw.
pub async fn detect_and_get_backend() -> Result<BackendDetectionResult, String> {
    if let Some(cached) = BACKEND_REGISTRY.lock().unwrap().cached_detection.clone() {
        return cached;
    }

    let inside_tmux = detection::is_inside_tmux().await;
    let in_iterm2 = detection::is_in_iterm2();
    let prefer_tmux_over_iterm2 = super::it2_setup::get_prefer_tmux_over_iterm2();

    // CC `:159-171` — inside tmux always wins.
    let result = if inside_tmux {
        Ok(BackendDetectionResult {
            backend_type: PaneBackendType::Tmux,
            is_native: true,
            needs_it2_setup: false,
        })
    } else if in_iterm2 {
        // CC `:174-231`.
        if !prefer_tmux_over_iterm2 && detection::is_it2_cli_available().await {
            Ok(BackendDetectionResult {
                backend_type: PaneBackendType::ITerm2,
                is_native: true,
                needs_it2_setup: false,
            })
        } else if detection::is_tmux_available().await {
            Ok(BackendDetectionResult {
                backend_type: PaneBackendType::Tmux,
                is_native: false,
                needs_it2_setup: !prefer_tmux_over_iterm2,
            })
        } else {
            // CC `:228-230`.
            Err(
                "iTerm2 detected but it2 CLI not installed. Install it2 with: pip install it2"
                    .to_string(),
            )
        }
    } else if detection::is_tmux_available().await {
        // CC `:239-249`.
        Ok(BackendDetectionResult {
            backend_type: PaneBackendType::Tmux,
            is_native: false,
            needs_it2_setup: false,
        })
    } else {
        // CC `:253`.
        Err(get_tmux_install_instructions().to_string())
    };

    BACKEND_REGISTRY.lock().unwrap().cached_detection = Some(result.clone());
    result
}

/// Maps to: CC `getTmuxInstallInstructions()`.
pub fn get_tmux_install_instructions() -> &'static str {
    match crate::utils::env::get().platform {
        crate::utils::env::Platform::MacOS => {
            "To use agent swarms, install tmux:\n  brew install tmux\nThen start a tmux session with: tmux new-session -s claude"
        }
        crate::utils::env::Platform::Linux => {
            "To use agent swarms, install tmux:\n  sudo apt install tmux    # Ubuntu/Debian\n  sudo dnf install tmux    # Fedora/RHEL\nThen start a tmux session with: tmux new-session -s claude"
        }
        crate::utils::env::Platform::Windows => {
            "To use agent swarms, you need tmux which requires WSL (Windows Subsystem for Linux).\nInstall WSL first, then inside WSL run:\n  sudo apt install tmux\nThen start a tmux session with: tmux new-session -s claude"
        }
    }
}

/// Maps to: CC `getPaneBackendExecutor()` cached executor construction.
pub fn get_pane_backend_executor_for_type(
    backend_type: PaneBackendType,
) -> Result<PaneBackendExecutor, String> {
    let mut state = BACKEND_REGISTRY.lock().unwrap();
    if let Some(executor) = state.cached_pane_executor.as_ref() {
        if executor.backend_type() == backend_type.as_backend_type() {
            return Ok(executor.clone());
        }
    }
    let executor = create_pane_backend_executor(backend_type)?;
    state.cached_pane_executor = Some(executor.clone());
    Ok(executor)
}

/// Best-effort pane kill for shutdown-approved teammates restored from team
/// file metadata.
/// Maps to: CC `useInboxPoller.ts` shutdown-approved backend `killPane` call.
pub fn spawn_kill_pane_for_shutdown(backend_type: Option<&str>, pane_id: String, agent_id: String) {
    let backend_type = backend_type.unwrap_or("tmux").to_ascii_lowercase();
    tokio::spawn(async move {
        let use_external_session = !detection::is_inside_tmux_sync();
        match backend_type.as_str() {
            "tmux" => {
                if let Ok(executor) = get_pane_backend_executor_for_type(PaneBackendType::Tmux) {
                    if executor.kill(&agent_id).await {
                        return;
                    }
                }
                let backend = super::tmux_backend::TmuxBackend::new();
                let _ = backend.kill_pane(&pane_id, use_external_session).await;
            }
            "iterm2" => {
                if let Ok(executor) = get_pane_backend_executor_for_type(PaneBackendType::ITerm2) {
                    if executor.kill(&agent_id).await {
                        return;
                    }
                }
                let backend = super::iterm_backend::ITermBackend::new();
                let _ = backend.kill_pane(&pane_id, use_external_session).await;
            }
            _ => {}
        }
    });
}

pub async fn get_resolved_pane_backend() -> Option<BackendDetectionResult> {
    match get_teammate_mode_from_snapshot() {
        TeammateMode::InProcess => None,
        TeammateMode::Tmux => {
            if detection::is_inside_tmux().await || detection::is_tmux_available().await {
                Some(BackendDetectionResult {
                    backend_type: PaneBackendType::Tmux,
                    is_native: detection::is_inside_tmux_sync(),
                    needs_it2_setup: false,
                })
            } else {
                None
            }
        }
        TeammateMode::Auto => detect_and_get_backend().await.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_process_enabled_matches_official_registry_decision_matrix() {
        let _lock = TEST_BACKEND_REGISTRY_LOCK.lock().unwrap();
        reset_backend_detection();

        assert!(is_in_process_enabled_from_parts(
            true,
            TeammateMode::Tmux,
            false,
            true,
            false
        ));
        assert!(is_in_process_enabled_from_parts(
            false,
            TeammateMode::InProcess,
            false,
            true,
            true
        ));
        assert!(is_in_process_enabled_from_parts(
            false,
            TeammateMode::Auto,
            true,
            true,
            true
        ));
        assert!(!is_in_process_enabled_from_parts(
            false,
            TeammateMode::Tmux,
            true,
            false,
            false
        ));
        assert!(!is_in_process_enabled_from_parts(
            false,
            TeammateMode::Auto,
            false,
            true,
            false
        ));
        assert!(!is_in_process_enabled_from_parts(
            false,
            TeammateMode::Auto,
            false,
            false,
            true
        ));
        assert!(is_in_process_enabled_from_parts(
            false,
            TeammateMode::Auto,
            false,
            false,
            false
        ));
    }

    #[test]
    fn pane_backend_executor_is_cached_like_official_registry() {
        let _lock = TEST_BACKEND_REGISTRY_LOCK.lock().unwrap();
        reset_backend_detection();
        let first = get_pane_backend_executor_for_type(PaneBackendType::Tmux).unwrap();
        let second = get_pane_backend_executor_for_type(PaneBackendType::Tmux).unwrap();
        assert_eq!(first.backend_type(), second.backend_type());
        reset_backend_detection();
    }

    #[test]
    fn mark_in_process_fallback_forces_in_process_like_official() {
        let _lock = TEST_BACKEND_REGISTRY_LOCK.lock().unwrap();
        reset_backend_detection();
        assert!(!is_in_process_enabled_from_parts(
            false,
            TeammateMode::Auto,
            false,
            true,
            false
        ));
        mark_in_process_fallback();
        assert!(is_in_process_enabled());
        reset_backend_detection();
    }
}
