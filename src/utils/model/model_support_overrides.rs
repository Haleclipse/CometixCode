//! Maps to: CC `utils/model/modelSupportOverrides.ts`.
//!
//! 3P (Bedrock/Vertex/Foundry) capability overrides pinned through the
//! `ANTHROPIC_DEFAULT_*_MODEL` / `ANTHROPIC_DEFAULT_*_MODEL_SUPPORTED_CAPABILITIES`
//! env pairs. Consumed by `utils/effort.rs`, `utils/thinking.rs` and
//! `utils/betas.rs` exactly where CC imports `get3PModelCapabilityOverride`.

use crate::utils::model::providers::{ApiProvider, get_api_provider};
use crate::utils::process_env::JsTruthy;

/// Maps to: CC `modelSupportOverrides.ts:4-9` `ModelCapabilityOverride`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelCapabilityOverride {
    Effort,
    MaxEffort,
    XhighEffort,
    Thinking,
    AdaptiveThinking,
    InterleavedThinking,
}

impl ModelCapabilityOverride {
    /// The literal CC compares against the comma-separated
    /// `*_SUPPORTED_CAPABILITIES` list.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Effort => "effort",
            Self::MaxEffort => "max_effort",
            Self::XhighEffort => "xhigh_effort",
            Self::Thinking => "thinking",
            Self::AdaptiveThinking => "adaptive_thinking",
            Self::InterleavedThinking => "interleaved_thinking",
        }
    }
}

/// Maps to: CC `modelSupportOverrides.ts:11-24` `TIERS`.
const TIERS: &[(&str, &str)] = &[
    (
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
    ),
    (
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
    ),
    (
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
    ),
];

/// Maps to: CC `modelSupportOverrides.ts:30-51` `get3PModelCapabilityOverride`.
///
/// Check whether a 3p model capability override is set for a model that
/// matches one of the pinned `ANTHROPIC_DEFAULT_*_MODEL` env vars.
///
/// - `firstParty` → `None` (CC `undefined`), the caller falls through to its
///   canonical-name rule.
/// - A tier only participates when its model env is non-empty (`!pinned`) and
///   its capabilities env is defined at all (`capabilities === undefined`), so
///   an empty capabilities list is a valid "supports nothing" pin.
///
/// CC wraps this in `memoize` keyed by `${model.toLowerCase()}:${capability}`;
/// the inputs are process env only, so the uncached read is equivalent.
pub fn get_3p_model_capability_override(
    model: &str,
    capability: ModelCapabilityOverride,
) -> Option<bool> {
    if get_api_provider() == ApiProvider::FirstParty {
        return None;
    }
    let m = model.to_lowercase();
    for (model_env_var, capabilities_env_var) in TIERS {
        let Some(pinned) = crate::utils::process_env::var(model_env_var).truthy() else {
            continue;
        };
        let Some(capabilities) = crate::utils::process_env::var(capabilities_env_var) else {
            continue;
        };
        if m != pinned.to_lowercase() {
            continue;
        }
        return Some(
            capabilities
                .to_lowercase()
                .split(',')
                .map(str::trim)
                .any(|item| item == capability.as_str()),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn first_party_returns_none_even_when_pinned() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _pinned = EnvVarGuard::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "my-sonnet");
        let _caps = EnvVarGuard::set(
            "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
            "thinking",
        );
        assert_eq!(
            get_3p_model_capability_override("my-sonnet", ModelCapabilityOverride::Thinking),
            None
        );
    }

    #[test]
    fn bedrock_pinned_tier_matches_official_case_insensitive_list() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _opus = EnvVarGuard::unset("ANTHROPIC_DEFAULT_OPUS_MODEL");
        let _haiku = EnvVarGuard::unset("ANTHROPIC_DEFAULT_HAIKU_MODEL");
        let _pinned = EnvVarGuard::set("ANTHROPIC_DEFAULT_SONNET_MODEL", "My-Sonnet");
        let _caps = EnvVarGuard::set(
            "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES",
            " Thinking, INTERLEAVED_THINKING ",
        );

        assert_eq!(
            get_3p_model_capability_override("my-sonnet", ModelCapabilityOverride::Thinking),
            Some(true)
        );
        assert_eq!(
            get_3p_model_capability_override(
                "my-sonnet",
                ModelCapabilityOverride::InterleavedThinking
            ),
            Some(true)
        );
        assert_eq!(
            get_3p_model_capability_override("my-sonnet", ModelCapabilityOverride::Effort),
            Some(false)
        );
        // Unpinned model → undefined, not false.
        assert_eq!(
            get_3p_model_capability_override("other-model", ModelCapabilityOverride::Thinking),
            None
        );
    }

    #[test]
    fn empty_capabilities_env_is_a_defined_empty_list() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _opus = EnvVarGuard::unset("ANTHROPIC_DEFAULT_OPUS_MODEL");
        let _sonnet = EnvVarGuard::unset("ANTHROPIC_DEFAULT_SONNET_MODEL");
        let _pinned = EnvVarGuard::set("ANTHROPIC_DEFAULT_HAIKU_MODEL", "my-haiku");
        let _caps = EnvVarGuard::set("ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES", "");
        assert_eq!(
            get_3p_model_capability_override("my-haiku", ModelCapabilityOverride::Thinking),
            Some(false)
        );
    }
}
