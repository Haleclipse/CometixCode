//! Shared analytics configuration.
//!
//! Maps to: CC `services/analytics/config.ts`.

/// Maps to: CC `services/analytics/config.ts:16-27` `isAnalyticsDisabled`.
pub fn is_analytics_disabled() -> bool {
    crate::utils::process_env::var("NODE_ENV").as_deref() == Some("test")
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_USE_BEDROCK").as_deref(),
        )
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_USE_VERTEX").as_deref(),
        )
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_USE_FOUNDRY").as_deref(),
        )
        || crate::utils::privacy_level::is_telemetry_disabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn analytics_disable_conditions_match_official_provider_and_privacy_gates() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keys = [
            "NODE_ENV",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "DISABLE_TELEMETRY",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ];
        for key in keys {
            crate::utils::process_env::remove(key);
        }
        assert!(!is_analytics_disabled());

        for (key, value) in [
            ("NODE_ENV", "test"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CLAUDE_CODE_USE_VERTEX", "1"),
            ("CLAUDE_CODE_USE_FOUNDRY", "1"),
            ("DISABLE_TELEMETRY", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ] {
            crate::utils::process_env::set(key, value);
            assert!(is_analytics_disabled(), "condition={key}");
            crate::utils::process_env::remove(key);
        }
    }
}
