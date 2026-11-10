//! Advisor feature and model predicates.
//!
//! Maps to CC `utils/advisor.ts`.  The server-side advisor transport already
//! lives at the API/message owners; this file owns only the shared feature
//! gate, model gates, and initial settings projection used by the
//! `/advisor` command and query builder.

/// Maps to CC `isAdvisorEnabled()` (`utils/advisor.ts:60-69`). @nyan: the
/// `tengu_sage_compass` config is the source-controlled
/// [`FeatureFlag::Advisor`](crate::utils::feature_flags::FeatureFlag::Advisor)
/// switch.
pub fn is_advisor_enabled() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_DISABLE_ADVISOR_TOOL").as_deref(),
    ) {
        return false;
    }
    if !crate::services::provider::anthropic::betas::should_include_first_party_only_betas() {
        return false;
    }
    crate::utils::feature_flags::feature_enabled(crate::utils::feature_flags::FeatureFlag::Advisor)
}

/// Maps to CC `canUserConfigureAdvisor()` (`utils/advisor.ts:71-73`). @nyan:
/// there are no server-assigned experiment advisor models, so an enabled
/// advisor is always user-configured.
pub fn can_user_configure_advisor() -> bool {
    is_advisor_enabled()
}

/// Maps to CC `modelSupportsAdvisor()` (`utils/advisor.ts:88-96`).
pub fn model_supports_advisor(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("opus-4-6") || model.contains("sonnet-4-6")
}

/// Maps to CC `isValidAdvisorModel()` (`utils/advisor.ts:98-106`).
pub fn is_valid_advisor_model(model: &str) -> bool {
    model_supports_advisor(model)
}

/// Maps to CC `getInitialAdvisorSetting()` (`utils/advisor.ts:108-113`).
pub fn get_initial_advisor_setting() -> Option<String> {
    is_advisor_enabled()
        .then(|| crate::utils::settings::get_initial_settings().advisor_model)
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn advisor_model_predicates_match_official_launch_allowlist() {
        assert!(model_supports_advisor("claude-opus-4-6"));
        assert!(model_supports_advisor("claude-sonnet-4-6-20260301"));
        assert!(!model_supports_advisor("claude-opus-4-5"));
        assert!(!is_valid_advisor_model("custom-enterprise-model"));
    }

    #[test]
    fn advisor_gate_follows_the_source_controlled_switch() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _disabled = EnvVarGuard::unset("NYAN_DISABLE_ADVISOR_TOOL");
        assert!(!crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::Advisor
        ));
        assert!(!is_advisor_enabled());
        assert!(!can_user_configure_advisor());
        assert_eq!(get_initial_advisor_setting(), None);
    }

    #[test]
    fn advisor_gate_honors_disable_environment_before_feature_config() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _disabled = EnvVarGuard::set("NYAN_DISABLE_ADVISOR_TOOL", "1");
        assert!(!is_advisor_enabled());
        assert!(!can_user_configure_advisor());
    }
}
