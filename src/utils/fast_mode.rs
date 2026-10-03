//! Fast-mode runtime gate and cooldown state.
//! Maps to CC `utils/fastMode.ts`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};

pub const FAST_MODE_MODEL_DISPLAY: &str = "Opus 4.6";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FastModeOrgStatus {
    #[default]
    Pending,
    Enabled,
    Disabled {
        reason: String,
    },
}

static ORG_STATUS: LazyLock<RwLock<FastModeOrgStatus>> =
    LazyLock::new(|| RwLock::new(FastModeOrgStatus::Pending));

static COOLDOWN_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Maps to CC's external fast-mode feature gate. The query configuration uses
/// the same disable environment variable; no entitlement/network probe occurs
/// here.
pub fn is_fast_mode_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_FAST_MODE")
            .as_deref(),
    )
}

pub fn set_fast_mode_org_status(status: FastModeOrgStatus) {
    if let Ok(mut current) = ORG_STATUS.write() {
        *current = status;
    }
}

pub fn get_fast_mode_org_status() -> FastModeOrgStatus {
    ORG_STATUS
        .read()
        .map(|status| status.clone())
        .unwrap_or_default()
}

/// Maps to CC `getFastModeUnavailableReason()` for locally knowable state.
/// The org endpoint remains service-owned: its result enters through
/// `set_fast_mode_org_status`, never through an implicit PromptInput request.
pub fn get_fast_mode_unavailable_reason() -> Option<String> {
    if !is_fast_mode_enabled() {
        return Some("Fast mode is not available".to_string());
    }
    if crate::utils::model::providers::get_api_provider()
        != crate::utils::model::providers::ApiProvider::FirstParty
    {
        return Some("Fast mode is not available on Bedrock, Vertex, or Foundry".to_string());
    }
    match get_fast_mode_org_status() {
        FastModeOrgStatus::Disabled { reason } => Some(reason),
        FastModeOrgStatus::Pending | FastModeOrgStatus::Enabled => None,
    }
}

pub fn is_fast_mode_available() -> bool {
    get_fast_mode_unavailable_reason().is_none()
}

pub fn is_fast_mode_supported_by_model(model: Option<&str>) -> bool {
    if !is_fast_mode_enabled() {
        return false;
    }
    let resolved = model
        .map(crate::utils::model::model::parse_user_specified_model)
        .unwrap_or_else(crate::utils::model::model::get_default_main_loop_model);
    resolved.to_ascii_lowercase().contains("opus-4-6")
}

/// Maps to: CC `utils/fastMode.ts:145-147` `getFastModeModel()` —
/// `'opus' + (isOpus1mMergeEnabled() ? '[1m]' : '')`. The gate reads process
/// state itself, exactly as at the source — the local config/env bindings this
/// used to build were left dead when the predicate's unused parameters were
/// removed.
pub fn get_fast_mode_model() -> String {
    if crate::utils::model::model::is_opus_1m_merge_enabled() {
        "opus[1m]".to_string()
    } else {
        "opus".to_string()
    }
}

/// Maps to CC `getInitialFastModeSetting(model)`; settings values are supplied
/// by the startup/main owner so this utility stays independent of the TUI.
pub fn get_initial_fast_mode_setting(
    model: Option<&str>,
    persisted_fast_mode: Option<bool>,
    per_session_opt_in: Option<bool>,
) -> bool {
    is_fast_mode_enabled()
        && is_fast_mode_available()
        && is_fast_mode_supported_by_model(model)
        && per_session_opt_in != Some(true)
        && persisted_fast_mode == Some(true)
}

pub fn is_fast_mode_cooldown() -> bool {
    COOLDOWN_UNTIL_MS.load(Ordering::Relaxed) > now_ms()
}

pub fn trigger_fast_mode_cooldown(duration: std::time::Duration) {
    COOLDOWN_UNTIL_MS.store(
        now_ms().saturating_add(duration.as_millis() as u64),
        Ordering::Relaxed,
    );
}

pub fn clear_fast_mode_cooldown() {
    COOLDOWN_UNTIL_MS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn model_support_matches_current_opus_4_6_contract() {
        assert!(is_fast_mode_supported_by_model(Some("opus")));
        assert!(is_fast_mode_supported_by_model(Some("claude-opus-4-6")));
        assert!(!is_fast_mode_supported_by_model(Some("sonnet")));
    }

    #[test]
    fn initial_setting_requires_persisted_opt_in_supported_model_and_no_session_gate() {
        set_fast_mode_org_status(FastModeOrgStatus::Pending);
        assert!(get_initial_fast_mode_setting(
            Some("opus"),
            Some(true),
            Some(false)
        ));
        assert!(!get_initial_fast_mode_setting(
            Some("sonnet"),
            Some(true),
            Some(false)
        ));
        assert!(!get_initial_fast_mode_setting(
            Some("opus"),
            Some(true),
            Some(true)
        ));
    }

    #[test]
    fn explicit_org_status_controls_availability_without_network_io() {
        set_fast_mode_org_status(FastModeOrgStatus::Disabled {
            reason: "Fast mode has been disabled by your organization".to_string(),
        });
        assert_eq!(
            get_fast_mode_unavailable_reason().as_deref(),
            Some("Fast mode has been disabled by your organization")
        );
        set_fast_mode_org_status(FastModeOrgStatus::Pending);
    }

    #[test]
    fn fast_mode_model_appends_1m_suffix_per_merge_gate() {
        // CC fastMode.ts:145-147 — `'opus' + (isOpus1mMergeEnabled() ? '[1m]' : '')`.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // Deterministic non-1m branch: the disable env forces the merge gate
        // false regardless of the on-disk global config.
        crate::utils::process_env::set("CLAUDE_CODE_DISABLE_1M_CONTEXT", "1");
        assert_eq!(get_fast_mode_model(), "opus");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        // The 1m branch depends on the real global config (subscription
        // state), so assert agreement with the production gate rather than
        // forcing an environment that may not be constructible here.
        let expected = if crate::utils::model::model::is_opus_1m_merge_enabled() {
            "opus[1m]"
        } else {
            "opus"
        };
        assert_eq!(get_fast_mode_model(), expected);
    }

    #[test]
    fn cooldown_runtime_state_can_be_triggered_and_cleared() {
        clear_fast_mode_cooldown();
        assert!(!is_fast_mode_cooldown());
        trigger_fast_mode_cooldown(std::time::Duration::from_secs(1));
        assert!(is_fast_mode_cooldown());
        clear_fast_mode_cooldown();
        assert!(!is_fast_mode_cooldown());
    }
}
