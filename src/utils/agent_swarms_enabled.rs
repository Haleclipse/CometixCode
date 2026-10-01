//! Maps to CC `utils/agentSwarmsEnabled.ts`.

/// Maps to: CC `isAgentSwarmsEnabled()`.
///
/// The external-user killswitch reads Cometix's hardcoded feature-switch
/// collection instead of GrowthBook cache/config/network state.
pub fn is_agent_swarms_enabled_for(
    audience: crate::utils::build_profile::BuildAudience,
    external_opt_in: bool,
    external_killswitch: bool,
) -> bool {
    if crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::AgentSwarms,
    ) {
        return true;
    }
    external_opt_in && external_killswitch
}

pub fn is_agent_swarms_enabled() -> bool {
    let external_opt_in = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS")
            .as_deref(),
    ) || std::env::args().any(|arg| arg == "--agent-teams");
    is_agent_swarms_enabled_for(
        crate::utils::build_profile::build_audience(),
        external_opt_in,
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::AgentSwarmsExternalKillswitch,
        ),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_agent_swarms_enabled_matches_official_internal_and_external_opt_in_gate() {
        use crate::utils::build_profile::BuildAudience;

        assert!(!super::is_agent_swarms_enabled_for(
            BuildAudience::External,
            false,
            true,
        ));
        assert!(super::is_agent_swarms_enabled_for(
            BuildAudience::AnthropicInternal,
            false,
            false,
        ));
        assert!(super::is_agent_swarms_enabled_for(
            BuildAudience::External,
            true,
            true,
        ));
        assert!(!super::is_agent_swarms_enabled_for(
            BuildAudience::External,
            true,
            false,
        ));
    }
}
