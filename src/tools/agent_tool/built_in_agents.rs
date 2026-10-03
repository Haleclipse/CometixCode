//! Built-in agent registry.
//!
//! Maps to CC `tools/AgentTool/builtInAgents.ts`.
//!
//! Individual built-in agent definitions live under `built_in/*`, matching CC
//! `tools/AgentTool/built-in/*`. This module only performs the official
//! registry/gating assembly.

use super::built_in::{
    claude_code_guide_agent::claude_code_guide_agent, explore_agent::explore_agent,
    general_purpose_agent::general_purpose_agent, plan_agent::plan_agent,
    statusline_setup::statusline_setup_agent, verification_agent::verification_agent,
};
use super::load_agents_dir::AgentDefinition;

/// Maps to CC `areExplorePlanAgentsEnabled()`.
///
/// Maps to: CC `builtInAgents.ts:13-20` `areExplorePlanAgentsEnabled` —
/// `feature('BUILTIN_EXPLORE_PLAN_AGENTS')` is ON in production builds
/// (`scripts/build.ts:44`), then the `tengu_amber_stoat` GrowthBook gate
/// (fallback true). GrowthBook delivery is out of scope for this port
/// (user ruling): the gate lives as a hardcoded switch-table entry.
pub fn are_explore_plan_agents_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BuiltinExplorePlanAgents,
    )
}

/// Maps to CC `getBuiltInAgents()` (`builtInAgents.ts:22-72`).
pub fn get_built_in_agents() -> Vec<AgentDefinition> {
    // Maps to official SDK/non-interactive escape hatch.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS").as_deref(),
    ) && crate::bootstrap::state::get_is_non_interactive_session()
    {
        return Vec::new();
    }

    let mut agents = vec![general_purpose_agent(), statusline_setup_agent()];

    if are_explore_plan_agents_enabled() {
        agents.extend([explore_agent(), plan_agent()]);
    }

    // Maps to official non-SDK entrypoint check.
    if !matches!(
        crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref(),
        Some("sdk-ts" | "sdk-py" | "sdk-cli")
    ) {
        agents.push(claude_code_guide_agent());
    }

    if is_verification_agent_enabled_readonly() {
        agents.push(verification_agent());
    }

    // The coordinator built-ins are feature/GrowthBook gated upstream and are
    // not materialized until those feature snapshots exist.
    agents
}

/// Maps to CC `builtInAgents.ts` verification feature/GrowthBook gate.
pub fn is_verification_agent_enabled_readonly() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::state::IsInteractiveGuard;
    use crate::types::permissions::PermissionMode;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Pins every input `getBuiltInAgents()` reads: the two variables, and the
    /// session's interactivity — `IS_INTERACTIVE` together with its
    /// `cfg(test)`-only env overrides (`bootstrap/state.rs`), so an ambient
    /// `CLAUDE_CODE_NON_INTERACTIVE` cannot turn the interactive case headless.
    /// The caller holds `TEST_ENV_LOCK`.
    fn pin_inputs(
        entrypoint: Option<&str>,
        disable_built_in_agents: Option<&str>,
        non_interactive: bool,
    ) -> (Vec<EnvVarGuard>, IsInteractiveGuard) {
        let pin = |key, value: Option<&str>| match value {
            Some(value) => EnvVarGuard::set(key, value),
            None => EnvVarGuard::unset(key),
        };
        let env = vec![
            pin("CLAUDE_CODE_ENTRYPOINT", entrypoint),
            pin(
                "CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS",
                disable_built_in_agents,
            ),
            EnvVarGuard::unset("CLAUDE_CODE_NON_INTERACTIVE"),
            EnvVarGuard::unset("COMETIX_NON_INTERACTIVE"),
            EnvVarGuard::unset("COMETIX_NON_INTERACTIVE_SESSION"),
        ];
        let interactive = IsInteractiveGuard::capture();
        crate::bootstrap::state::set_is_interactive(!non_interactive);
        (env, interactive)
    }

    #[test]
    fn built_in_agents_match_official_default_and_sdk_entrypoint_gate() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        // CC builtInAgents.ts:45-52: base pair, then Explore/Plan (gate ON in
        // production external builds — build.ts:44 + tengu_amber_stoat
        // fallback true), then guide for non-SDK entrypoints.
        let default = {
            let _inputs = pin_inputs(None, None, false);
            get_built_in_agents()
        };
        assert_eq!(
            default
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "general-purpose",
                "statusline-setup",
                "Explore",
                "Plan",
                "claude-code-guide"
            ]
        );

        // The SDK entrypoint drops only the guide (:55-62); Explore/Plan stay.
        let sdk = {
            let _inputs = pin_inputs(Some("sdk-ts"), None, false);
            get_built_in_agents()
        };
        assert_eq!(
            sdk.iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["general-purpose", "statusline-setup", "Explore", "Plan"]
        );
    }

    #[test]
    fn built_in_agents_honor_noninteractive_sdk_disable_gate() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let disabled = {
            let _inputs = pin_inputs(None, Some("true"), true);
            get_built_in_agents()
        };
        assert!(disabled.is_empty());

        let interactive = {
            let _inputs = pin_inputs(None, Some("true"), false);
            get_built_in_agents()
        };
        assert!(!interactive.is_empty());
    }

    #[test]
    fn built_in_agent_registry_assembles_official_agent_modules() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let agents = {
            let _inputs = pin_inputs(None, None, false);
            get_built_in_agents()
        };
        let statusline = agents
            .iter()
            .find(|agent| agent.agent_type == "statusline-setup")
            .unwrap();
        assert_eq!(statusline.model.as_deref(), Some("sonnet"));
        assert_eq!(
            statusline.tools.as_deref(),
            Some(&["Read".to_string(), "Edit".to_string()][..])
        );

        let guide = agents
            .iter()
            .find(|agent| agent.agent_type == "claude-code-guide")
            .unwrap();
        assert_eq!(guide.model.as_deref(), Some("haiku"));
        assert_eq!(guide.permission_mode, Some(PermissionMode::DontAsk));
    }
}
