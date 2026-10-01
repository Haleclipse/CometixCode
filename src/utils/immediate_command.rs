//! Maps to: CC `utils/immediateCommand.ts`.
//!
//! Whether inference-config commands (`/model`, `/fast`, `/effort`) execute
//! immediately during a running query rather than waiting for the current turn
//! to finish.

use crate::utils::config::{GlobalConfig, load_global_config};

pub fn should_inference_config_command_be_immediate_from(
    audience: crate::utils::build_profile::BuildAudience,
    hardcoded_feature_enabled: bool,
) -> bool {
    crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Commands,
    ) || hardcoded_feature_enabled
}

pub fn should_inference_config_command_be_immediate_from_config(
    _config: &GlobalConfig,
    _get_env: &impl Fn(&str) -> Option<String>,
) -> bool {
    // Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_immediate_model_command', false)`.
    // Per Cometix policy, GrowthBook cache/config/env overrides are not read here;
    // the external-user branch is controlled only by `utils::feature_flags`.
    should_inference_config_command_be_immediate_from(
        crate::utils::build_profile::build_audience(),
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::ImmediateModelCommand,
        ),
    )
}

/// Maps to: CC `utils/immediateCommand.ts:10-15`
/// `shouldInferenceConfigCommandBeImmediate`.
pub fn should_inference_config_command_be_immediate() -> bool {
    let config = load_global_config();
    should_inference_config_command_be_immediate_from_config(&config, &|key| {
        crate::utils::process_env::var(key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_model_command_gate_uses_hardcoded_feature_switch_not_growthbook_cache() {
        let config: GlobalConfig = serde_json::from_value(serde_json::json!({
            "cachedGrowthBookFeatures": {"tengu_immediate_model_command": true},
            "growthBookOverrides": {"tengu_immediate_model_command": true}
        }))
        .expect("unknown GrowthBook cache fields should be ignored by config parsing");

        assert!(should_inference_config_command_be_immediate_from(
            crate::utils::build_profile::BuildAudience::External,
            true
        ));
        assert_eq!(
            should_inference_config_command_be_immediate_from_config(&config, &|_| None),
            crate::utils::build_profile::build_audience().is_internal(),
            "GrowthBook cache/overrides are intentionally ignored; external enablement is source-controlled in utils::feature_flags"
        );
    }

    #[test]
    fn immediate_model_command_gate_keeps_internal_build_fast_path() {
        assert!(should_inference_config_command_be_immediate_from(
            crate::utils::build_profile::BuildAudience::AnthropicInternal,
            false,
        ));
    }
}
