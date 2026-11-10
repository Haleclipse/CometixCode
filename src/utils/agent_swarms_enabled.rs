//! Maps to CC `utils/agentSwarmsEnabled.ts`.

/// Maps to: CC `isAgentSwarmsEnabled()`.
///
/// The external-user killswitch reads Nyan's hardcoded feature-switch
/// collection instead of GrowthBook cache/config/network state.
pub fn is_agent_swarms_enabled() -> bool {
    let external_opt_in = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("NYAN_EXPERIMENTAL_AGENT_TEAMS").as_deref(),
    ) || std::env::args().any(|arg| arg == "--agent-teams");
    external_opt_in
        && crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::AgentSwarmsExternalKillswitch,
        )
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn is_agent_swarms_enabled_matches_official_external_opt_in_gate() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        {
            let _off = EnvVarGuard::unset("NYAN_EXPERIMENTAL_AGENT_TEAMS");
            assert!(!super::is_agent_swarms_enabled());
        }
        let _on = EnvVarGuard::set("NYAN_EXPERIMENTAL_AGENT_TEAMS", "1");
        assert!(super::is_agent_swarms_enabled());
    }
}
