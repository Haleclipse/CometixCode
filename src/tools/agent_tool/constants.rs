//! Maps to CC `tools/AgentTool/constants.ts`.

pub const AGENT_TOOL_NAME: &str = "Agent";
pub const LEGACY_AGENT_TOOL_NAME: &str = "Task";
pub const REVIEW_AGENT_TYPE: &str = "review";

pub const DEFAULT_MAX_SUBAGENT_SPAWN_DEPTH: usize = 3;
pub const DEFAULT_MAX_CONCURRENT_SUBAGENTS: usize = 20;

/// Returns the maximum subagent nesting depth (default 3, env NYAN_MAX_SUBAGENT_SPAWN_DEPTH).
pub fn max_subagent_spawn_depth() -> usize {
    crate::utils::process_env::var("NYAN_MAX_SUBAGENT_SPAWN_DEPTH")
        .and_then(|val| val.parse().ok())
        .unwrap_or(DEFAULT_MAX_SUBAGENT_SPAWN_DEPTH)
}

/// Returns the maximum concurrent running subagents limit (default 20, env NYAN_MAX_CONCURRENT_SUBAGENTS).
pub fn max_concurrent_subagents() -> usize {
    crate::utils::process_env::var("NYAN_MAX_CONCURRENT_SUBAGENTS")
        .and_then(|val| val.parse().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT_SUBAGENTS)
}

/// Maps to CC `tools/AgentTool/constants.ts#ONE_SHOT_BUILTIN_AGENT_TYPES`.
pub const ONE_SHOT_BUILTIN_AGENT_TYPES: &[&str] = &["explore", "plan"];

/// Maps to CC `ONE_SHOT_BUILTIN_AGENT_TYPES.has(agentType)`.
pub fn is_one_shot_builtin_agent_type(agent_type: &str) -> bool {
    ONE_SHOT_BUILTIN_AGENT_TYPES.contains(&agent_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn run_limits_use_defaults_and_valid_environment_overrides() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _depth = EnvVarGuard::set("NYAN_MAX_SUBAGENT_SPAWN_DEPTH", "7");
        let _concurrency = EnvVarGuard::set("NYAN_MAX_CONCURRENT_SUBAGENTS", "42");
        assert_eq!(max_subagent_spawn_depth(), 7);
        assert_eq!(max_concurrent_subagents(), 42);
    }

    #[test]
    fn run_limits_ignore_invalid_environment_overrides() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _depth = EnvVarGuard::set("NYAN_MAX_SUBAGENT_SPAWN_DEPTH", "not-a-number");
        let _concurrency = EnvVarGuard::set("NYAN_MAX_CONCURRENT_SUBAGENTS", "0x20");
        assert_eq!(max_subagent_spawn_depth(), DEFAULT_MAX_SUBAGENT_SPAWN_DEPTH);
        assert_eq!(max_concurrent_subagents(), DEFAULT_MAX_CONCURRENT_SUBAGENTS);
    }
}
