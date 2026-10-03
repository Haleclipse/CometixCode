//! Advisor feature and model predicates.
//!
//! Maps to CC `utils/advisor.ts`.  The server-side advisor transport already
//! lives at the API/message owners; this file owns only the shared feature
//! configuration, model gates, and initial settings projection used by the
//! `/advisor` command and query builder.

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct AdvisorConfig {
    enabled: Option<bool>,
    can_user_configure: Option<bool>,
    base_model: Option<String>,
    advisor_model: Option<String>,
}

/// Maps to CC `getAdvisorConfig()` (`utils/advisor.ts:53-58`).
fn get_advisor_config() -> AdvisorConfig {
    crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
        "tengu_sage_compass",
        AdvisorConfig::default(),
    )
}

/// Maps to CC `isAdvisorEnabled()` (`utils/advisor.ts:60-69`).
pub fn is_advisor_enabled() -> bool {
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_ADVISOR_TOOL")
            .as_deref(),
    ) {
        return false;
    }
    if !crate::utils::betas::should_include_first_party_only_betas() {
        return false;
    }
    get_advisor_config().enabled.unwrap_or(false)
}

/// Maps to CC `canUserConfigureAdvisor()` (`utils/advisor.ts:71-73`).
pub fn can_user_configure_advisor() -> bool {
    is_advisor_enabled() && get_advisor_config().can_user_configure.unwrap_or(false)
}

/// Maps to CC `getExperimentAdvisorModels()` (`utils/advisor.ts:75-85`).
pub fn get_experiment_advisor_models() -> Option<(String, String)> {
    let config = get_advisor_config();
    (is_advisor_enabled()
        && !can_user_configure_advisor()
        && config.base_model.is_some()
        && config.advisor_model.is_some())
    .then(|| (config.base_model.unwrap(), config.advisor_model.unwrap()))
}

/// Maps to CC `modelSupportsAdvisor()` (`utils/advisor.ts:88-96`).
pub fn model_supports_advisor(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("opus-4-6")
        || model.contains("sonnet-4-6")
        || crate::utils::process_env::var("USER_TYPE").as_deref() == Some("ant")
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
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        assert!(model_supports_advisor("claude-opus-4-6"));
        assert!(model_supports_advisor("claude-sonnet-4-6-20260301"));
        assert!(!model_supports_advisor("claude-opus-4-5"));
        let _ant = EnvVarGuard::set("USER_TYPE", "ant");
        assert!(is_valid_advisor_model("custom-enterprise-model"));
    }

    #[test]
    fn advisor_gate_honors_disable_environment_before_feature_config() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _disabled = EnvVarGuard::set("CLAUDE_CODE_DISABLE_ADVISOR_TOOL", "1");
        assert!(!is_advisor_enabled());
        assert!(!can_user_configure_advisor());
    }
}
