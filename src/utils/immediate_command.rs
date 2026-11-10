//! Maps to: CC `utils/immediateCommand.ts`.
//!
//! Whether inference-config commands (`/model`, `/effort`) execute
//! immediately during a running query rather than waiting for the current turn
//! to finish.

/// Maps to: CC `utils/immediateCommand.ts:10-15`
/// `shouldInferenceConfigCommandBeImmediate`.
///
/// CC reads `getFeatureValue_CACHED_MAY_BE_STALE('tengu_immediate_model_command', false)`.
/// Per Nyan policy, GrowthBook cache/config/env overrides are not read here;
/// enablement is controlled only by `utils::feature_flags`.
pub fn should_inference_config_command_be_immediate() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::ImmediateModelCommand,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_model_command_gate_uses_hardcoded_feature_switch() {
        assert!(
            !should_inference_config_command_be_immediate(),
            "enablement is source-controlled in utils::feature_flags"
        );
    }
}
