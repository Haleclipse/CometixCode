//! Compile-time distribution profile and internal-capability projection.
//!
//! Maps to CC runtime checks of `process.env.USER_TYPE`, transformed for Rust
//! into a Cargo build feature. External is the safe/default distribution;
//! `--features anthropic_internal` produces the Anthropic-internal build.
//!
//! L1 contract: production code must not infer distribution identity from the
//! process environment. Consumers ask for a semantic capability so future
//! internal capabilities can be separated without another repository-wide
//! audience-check migration.

/// Compile-time distribution audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildAudience {
    External,
    AnthropicInternal,
}

impl BuildAudience {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::AnthropicInternal => "ant",
        }
    }

    pub const fn is_external(self) -> bool {
        matches!(self, Self::External)
    }

    pub const fn is_internal(self) -> bool {
        matches!(self, Self::AnthropicInternal)
    }
}

/// Named capabilities that CC gates on its internal-user distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InternalCapability {
    AgentSwarms,
    Api,
    Authentication,
    Commands,
    Context,
    ManagedConfiguration,
    Models,
    Permissions,
    Prompts,
    Speculation,
    InternalDiagnostics,
    Tools,
    Ui,
}

/// The immutable audience for this binary.
pub const fn build_audience() -> BuildAudience {
    if cfg!(feature = "anthropic_internal") {
        BuildAudience::AnthropicInternal
    } else {
        BuildAudience::External
    }
}

/// Pure capability projection used by tests that need to cover both profiles.
pub const fn audience_has_internal_capability(
    audience: BuildAudience,
    _capability: InternalCapability,
) -> bool {
    audience.is_internal()
}

/// Returns whether this binary includes the requested internal capability.
pub const fn has_internal_capability(capability: InternalCapability) -> bool {
    audience_has_internal_capability(build_audience(), capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_is_the_safe_default_unless_internal_cargo_feature_is_enabled() {
        assert_eq!(
            build_audience(),
            if cfg!(feature = "anthropic_internal") {
                BuildAudience::AnthropicInternal
            } else {
                BuildAudience::External
            }
        );
    }

    #[test]
    fn typed_capability_projection_covers_both_distribution_profiles() {
        for capability in [
            InternalCapability::AgentSwarms,
            InternalCapability::Api,
            InternalCapability::Authentication,
            InternalCapability::Commands,
            InternalCapability::Context,
            InternalCapability::ManagedConfiguration,
            InternalCapability::Models,
            InternalCapability::Permissions,
            InternalCapability::Prompts,
            InternalCapability::Speculation,
            InternalCapability::InternalDiagnostics,
            InternalCapability::Tools,
            InternalCapability::Ui,
        ] {
            assert!(!audience_has_internal_capability(
                BuildAudience::External,
                capability
            ));
            assert!(audience_has_internal_capability(
                BuildAudience::AnthropicInternal,
                capability
            ));
        }
    }

    #[test]
    fn persisted_audience_strings_remain_cc_compatible() {
        assert_eq!(BuildAudience::External.as_str(), "external");
        assert_eq!(BuildAudience::AnthropicInternal.as_str(), "ant");
    }
}
