//! Maps to: CC `utils/fullscreen.ts`.
//!
//! This module owns the runtime gates for the official fullscreen/alternate-
//! screen path. It intentionally contains no UI: `components/fullscreen_layout.rs`
//! consumes these booleans, while REPL/root wiring decides whether to mount the
//! alternate-screen shell.

use crate::bootstrap::state::get_is_interactive;
use std::process::Command;
use std::sync::{LazyLock, Mutex};

static TMUX_CONTROL_MODE_PROBED: LazyLock<Mutex<Option<bool>>> = LazyLock::new(|| Mutex::new(None));
static LOGGED_TMUX_CC_DISABLE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

/// Maps to: CC `utils/fullscreen.ts#isTmuxControlMode` env heuristic.
pub fn is_tmux_control_mode_env_heuristic_from_env(
    get_env: &impl Fn(&str) -> Option<String>,
) -> bool {
    if get_env("TMUX").is_none() {
        return false;
    }
    if get_env("TERM_PROGRAM").as_deref() != Some("iTerm.app") {
        return false;
    }
    let term = get_env("TERM").unwrap_or_default();
    !term.starts_with("screen") && !term.starts_with("tmux")
}

fn probe_tmux_control_mode_sync(get_env: &impl Fn(&str) -> Option<String>) -> bool {
    let heuristic = is_tmux_control_mode_env_heuristic_from_env(get_env);
    if heuristic {
        return true;
    }
    if get_env("TMUX").is_none() {
        return false;
    }
    // Official only spends a subprocess when iTerm might be involved: local
    // iTerm is covered by the heuristic above, SSH frequently drops
    // TERM_PROGRAM, and explicit non-iTerm TERM_PROGRAMs cannot be tmux -CC.
    if get_env("TERM_PROGRAM").is_some() {
        return false;
    }

    let mut command = Command::new("tmux");
    // CC inherits process.env (spawnSync without env); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let Ok(output) = command
        .args(["display-message", "-p", "#{client_control_mode}"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).trim() == "1"
}

/// Maps to: CC `utils/fullscreen.ts#isTmuxControlMode`.
pub fn is_tmux_control_mode() -> bool {
    let mut cached = TMUX_CONTROL_MODE_PROBED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(value) = *cached {
        return value;
    }
    let value = probe_tmux_control_mode_sync(&|key| crate::utils::process_env::var(key));
    *cached = Some(value);
    value
}

/// Pure audience-injected form of CC `utils/fullscreen.ts#isFullscreenEnvEnabled`.
pub fn is_fullscreen_env_enabled_for_audience(
    get_env: &impl Fn(&str) -> Option<String>,
    is_tmux_control_mode: bool,
    audience: crate::utils::build_profile::BuildAudience,
) -> bool {
    let no_flicker = get_env("CLAUDE_CODE_NO_FLICKER");
    if crate::utils::env_utils::is_env_defined_falsy(no_flicker.as_deref()) {
        return false;
    }
    if crate::utils::env_utils::is_env_truthy(no_flicker.as_deref()) {
        return true;
    }
    if is_tmux_control_mode {
        return false;
    }
    crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Ui,
    )
}

/// Maps to: CC `utils/fullscreen.ts:106-135` `isFullscreenEnvEnabled`.
pub fn is_fullscreen_env_enabled() -> bool {
    let tmux_control_mode = is_tmux_control_mode();
    let enabled = is_fullscreen_env_enabled_for_audience(
        &|key| crate::utils::process_env::var(key),
        tmux_control_mode,
        crate::utils::build_profile::build_audience(),
    );
    if tmux_control_mode && !enabled {
        let mut logged = LOGGED_TMUX_CC_DISABLE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !*logged {
            *logged = true;
            eprintln!(
                "fullscreen disabled: tmux -CC (iTerm2 integration mode) detected · set CLAUDE_CODE_NO_FLICKER=1 to override"
            );
        }
    }
    enabled
}

/// Maps to: CC `utils/fullscreen.ts:137-149` `isMouseTrackingEnabled`.
pub fn is_mouse_tracking_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_MOUSE").as_deref(),
    )
}

/// Maps to: CC `utils/fullscreen.ts:151-160` `isMouseClicksDisabled`.
pub fn is_mouse_clicks_disabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_MOUSE_CLICKS")
            .as_deref(),
    )
}

/// Maps to: CC `utils/fullscreen.ts:162-164` `isFullscreenActive`.
pub fn is_fullscreen_active() -> bool {
    get_is_interactive() && is_fullscreen_env_enabled()
}

/// Test-only reset for module-level once-per-session flags.
pub fn reset_for_testing() {
    *TMUX_CONTROL_MODE_PROBED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *LOGGED_TMUX_CC_DISABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct InteractiveGuard(bool);

    impl InteractiveGuard {
        fn set(value: bool) -> Self {
            let previous = crate::bootstrap::state::get_is_interactive();
            crate::bootstrap::state::set_is_interactive(value);
            Self(previous)
        }
    }

    impl Drop for InteractiveGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_is_interactive(self.0);
        }
    }

    struct FullscreenStateGuard;

    impl FullscreenStateGuard {
        fn reset() -> Self {
            reset_for_testing();
            Self
        }
    }

    impl Drop for FullscreenStateGuard {
        fn drop(&mut self) {
            reset_for_testing();
        }
    }

    fn env<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            entries
                .iter()
                .find_map(|(candidate, value)| (*candidate == key).then(|| (*value).to_string()))
        }
    }

    #[test]
    fn fullscreen_env_enabled_matches_official_internal_default_and_overrides() {
        use crate::utils::build_profile::BuildAudience;

        assert!(is_fullscreen_env_enabled_for_audience(
            &env(&[]),
            false,
            BuildAudience::AnthropicInternal,
        ));
        assert!(!is_fullscreen_env_enabled_for_audience(
            &env(&[]),
            false,
            BuildAudience::External,
        ));
        assert!(is_fullscreen_env_enabled_for_audience(
            &env(&[("CLAUDE_CODE_NO_FLICKER", "1")]),
            true,
            BuildAudience::External,
        ));
        assert!(!is_fullscreen_env_enabled_for_audience(
            &env(&[("CLAUDE_CODE_NO_FLICKER", "0")]),
            false,
            BuildAudience::AnthropicInternal,
        ));
        assert!(!is_fullscreen_env_enabled_for_audience(
            &env(&[]),
            true,
            BuildAudience::AnthropicInternal,
        ));
    }

    #[test]
    fn tmux_control_mode_heuristic_matches_iterm_cc_mode() {
        assert!(is_tmux_control_mode_env_heuristic_from_env(&env(&[
            ("TMUX", "/tmp/tmux"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("TERM", "xterm-256color"),
        ])));
        assert!(!is_tmux_control_mode_env_heuristic_from_env(&env(&[
            ("TMUX", "/tmp/tmux"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("TERM", "screen-256color"),
        ])));
        assert!(!is_tmux_control_mode_env_heuristic_from_env(&env(&[
            ("TMUX", "/tmp/tmux"),
            ("TERM_PROGRAM", "WezTerm"),
            ("TERM", "xterm-256color"),
        ])));
    }

    #[test]
    fn canonical_fullscreen_gates_match_official_process_state() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _state = FullscreenStateGuard::reset();
        let _no_flicker = EnvVarGuard::unset("CLAUDE_CODE_NO_FLICKER");
        let _disable_mouse = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_MOUSE");
        let _disable_clicks = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_MOUSE_CLICKS");
        let _tmux = EnvVarGuard::unset("TMUX");
        let _term_program = EnvVarGuard::unset("TERM_PROGRAM");
        let _interactive = InteractiveGuard::set(true);

        crate::utils::process_env::set("CLAUDE_CODE_NO_FLICKER", "1");
        assert!(is_fullscreen_env_enabled());
        assert!(is_fullscreen_active());
        crate::utils::process_env::set("CLAUDE_CODE_NO_FLICKER", "0");
        assert!(!is_fullscreen_env_enabled());
        assert!(!is_fullscreen_active());

        assert!(is_mouse_tracking_enabled());
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_MOUSE", "yes");
        assert!(!is_mouse_tracking_enabled());
        assert!(!is_mouse_clicks_disabled());
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_MOUSE_CLICKS", "on");
        assert!(is_mouse_clicks_disabled());

        crate::bootstrap::state::set_is_interactive(false);
        crate::utils::process_env::set("CLAUDE_CODE_NO_FLICKER", "1");
        assert!(!is_fullscreen_active());
    }
}
