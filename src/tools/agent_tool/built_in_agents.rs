//! Built-in agent registry.
//!
//! Individual built-in agent definitions live under `built_in/*`.
//! Nyan keeps four built-in agents: general-purpose, explore, plan, review.

use super::built_in::{
    explore_agent::explore_agent, general_purpose_agent::general_purpose_agent,
    plan_agent::plan_agent, review_agent::review_agent,
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

/// Maps to CC `getBuiltInAgents()` (`builtInAgents.ts:22-72`), cut down to
/// nyan's four: general-purpose, explore, plan and review. The statusline and
/// claude-code-guide agents are gone, and review replaces verification
/// unconditionally.
pub fn get_built_in_agents() -> Vec<AgentDefinition> {
    // Maps to official SDK/non-interactive escape hatch.
    if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS").as_deref(),
    ) && crate::bootstrap::state::get_is_non_interactive_session()
    {
        return Vec::new();
    }

    let mut agents = vec![general_purpose_agent()];
    if are_explore_plan_agents_enabled() {
        agents.extend([explore_agent(), plan_agent()]);
    }
    agents.push(review_agent());
    agents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::state::IsInteractiveGuard;
    use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Pins every input `getBuiltInAgents()` reads: the one variable, and the
    /// session's interactivity — `IS_INTERACTIVE` together with its
    /// `cfg(test)`-only env overrides (`bootstrap/state.rs`), so an ambient
    /// `CLAUDE_CODE_NON_INTERACTIVE` cannot turn the interactive case headless.
    /// The caller holds `TEST_ENV_LOCK`.
    fn pin_inputs(
        disable_built_in_agents: Option<&str>,
        non_interactive: bool,
    ) -> (Vec<EnvVarGuard>, IsInteractiveGuard) {
        let pin = |key, value: Option<&str>| match value {
            Some(value) => EnvVarGuard::set(key, value),
            None => EnvVarGuard::unset(key),
        };
        let env = vec![
            pin(
                "CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS",
                disable_built_in_agents,
            ),
            EnvVarGuard::unset("NYAN_NON_INTERACTIVE"),
            EnvVarGuard::unset("NYAN_NON_INTERACTIVE"),
            EnvVarGuard::unset("NYAN_NON_INTERACTIVE_SESSION"),
        ];
        let interactive = IsInteractiveGuard::capture();
        crate::bootstrap::state::set_is_interactive(!non_interactive);
        (env, interactive)
    }

    #[test]
    fn built_in_agents_are_exactly_the_four_nyan_types() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let agents = {
            let _inputs = pin_inputs(None, false);
            get_built_in_agents()
        };
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["general-purpose", "explore", "plan", "review"]
        );
        assert!(
            agents
                .iter()
                .all(|agent| agent.source == AgentDefinitionSource::BuiltIn)
        );
    }

    #[test]
    fn removed_builtins_are_not_registered() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let agents = {
            let _inputs = pin_inputs(None, false);
            get_built_in_agents()
        };
        for removed in ["statusline-setup", "claude-code-guide", "verification"] {
            assert!(
                agents.iter().all(|agent| agent.agent_type != removed),
                "{removed} must not be a built-in agent"
            );
        }
    }

    #[test]
    fn built_in_agents_honor_noninteractive_sdk_disable_gate() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let disabled = {
            let _inputs = pin_inputs(Some("true"), true);
            get_built_in_agents()
        };
        assert!(disabled.is_empty());

        let interactive = {
            let _inputs = pin_inputs(Some("true"), false);
            get_built_in_agents()
        };
        assert_eq!(interactive.len(), 4);
    }

    #[test]
    fn built_in_agent_registry_assembles_expected_agent_modules() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let agents = {
            let _inputs = pin_inputs(None, false);
            get_built_in_agents()
        };
        let review = agents
            .iter()
            .find(|agent| agent.agent_type == "review")
            .unwrap();
        assert_eq!(review.color.as_deref(), Some("red"));
        assert!(review.background);
        // Review is read-only: it cannot edit, write, or spawn agents.
        let disallowed = review.disallowed_tools.as_deref().unwrap_or_default();
        assert!(disallowed.iter().any(|tool| tool == "Agent"));
        assert!(disallowed.iter().any(|tool| tool == "Edit"));
        assert!(disallowed.iter().any(|tool| tool == "Write"));

        let explore = agents
            .iter()
            .find(|agent| agent.agent_type == "explore")
            .unwrap();
        assert!(explore.omit_claude_md);
    }

    /// Only `general-purpose` and `plan` follow the session; `explore` and
    /// `review` take the `@light` / `@heavy` tiers from the type defaults, so
    /// no built-in carries a model of its own that would shadow
    /// `selection.subagents.<type>`.
    #[test]
    fn built_in_agents_leave_the_model_to_the_selection_chain() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let agents = {
            let _inputs = pin_inputs(None, false);
            get_built_in_agents()
        };
        for agent in &agents {
            assert_eq!(agent.model, None, "{}", agent.agent_type);
        }
        let default_spec =
            |agent_type| crate::utils::model::agent::default_subagent_spec(agent_type);
        assert_eq!(default_spec("general-purpose"), "inherit");
        assert_eq!(default_spec("plan"), "inherit");
        assert_eq!(default_spec("explore"), "@light");
        assert_eq!(default_spec("review"), "@heavy");
    }
}
