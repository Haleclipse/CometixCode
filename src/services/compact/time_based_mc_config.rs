//! Maps to CC `services/compact/timeBasedMCConfig.ts`.

#[derive(Debug, Clone, PartialEq)]
pub struct TimeBasedMicrocompactConfig {
    pub enabled: bool,
    pub gap_threshold_minutes: f64,
    pub keep_recent: usize,
}

impl Default for TimeBasedMicrocompactConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            gap_threshold_minutes: 60.0,
            keep_recent: 5,
        }
    }
}

pub(crate) fn time_based_microcompact_config_from_env() -> TimeBasedMicrocompactConfig {
    let mut config = TimeBasedMicrocompactConfig::default();
    // Maps to CC GrowthBook `tengu_slate_heron`, but reads Cometix's
    // source-controlled feature switch collection instead of GrowthBook cache.
    config.enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TimeBasedMicrocompact,
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_TIME_BASED_MICROCOMPACT")
            .ok()
            .as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_TIME_BASED_MICROCOMPACT")
            .ok()
            .as_deref(),
    );
    if let Ok(value) = crate::utils::process_env::var("COMETIX_TIME_BASED_MICROCOMPACT_GAP_MINUTES")
    {
        if let Ok(minutes) = value.parse::<f64>() {
            config.gap_threshold_minutes = minutes;
        }
    }
    if let Ok(value) = crate::utils::process_env::var("COMETIX_TIME_BASED_MICROCOMPACT_KEEP_RECENT")
    {
        if let Ok(keep_recent) = value.parse::<usize>() {
            config.keep_recent = keep_recent;
        }
    }
    config
}

#[cfg(test)]
mod tests {
    #[test]
    fn time_based_microcompact_reads_hardcoded_switch_and_local_env_not_growthbook_cache() {
        let _env_guard = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("COMETIX_TIME_BASED_MICROCOMPACT");
        crate::utils::process_env::remove("CLAUDE_CODE_TIME_BASED_MICROCOMPACT");
        let default_config = super::time_based_microcompact_config_from_env();
        assert_eq!(
            default_config.enabled,
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::TimeBasedMicrocompact,
            )
        );

        crate::utils::process_env::set("COMETIX_TIME_BASED_MICROCOMPACT", "1");
        assert!(super::time_based_microcompact_config_from_env().enabled);
        crate::utils::process_env::remove("COMETIX_TIME_BASED_MICROCOMPACT");
    }
}
