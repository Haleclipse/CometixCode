//! Swarm constants.
//!
//! Maps to: CC `utils/swarm/constants.ts`.

/// Maps to: CC `utils/swarm/constants.ts#TEAM_LEAD_NAME`.
pub const TEAM_LEAD_NAME: &str = "team-lead";
/// Maps to: CC `utils/swarm/constants.ts#SWARM_SESSION_NAME`.
pub const SWARM_SESSION_NAME: &str = "claude-swarm";
/// Maps to: CC `utils/swarm/constants.ts#SWARM_VIEW_WINDOW_NAME`.
pub const SWARM_VIEW_WINDOW_NAME: &str = "swarm-view";
/// Maps to: CC `utils/swarm/constants.ts#TMUX_COMMAND`.
pub const TMUX_COMMAND: &str = "tmux";
/// Maps to: CC `utils/swarm/constants.ts#HIDDEN_SESSION_NAME`.
pub const HIDDEN_SESSION_NAME: &str = "claude-hidden";
/// Maps to: CC `utils/swarm/constants.ts#TEAMMATE_COMMAND_ENV_VAR`.
pub const TEAMMATE_COMMAND_ENV_VAR: &str = "NYAN_TEAMMATE_COMMAND";
/// Maps to: CC `utils/swarm/constants.ts#TEAMMATE_COLOR_ENV_VAR`.
pub const TEAMMATE_COLOR_ENV_VAR: &str = "NYAN_AGENT_COLOR";
/// Maps to: CC `utils/swarm/constants.ts#PLAN_MODE_REQUIRED_ENV_VAR`.
pub const PLAN_MODE_REQUIRED_ENV_VAR: &str = "NYAN_PLAN_MODE_REQUIRED";

/// Maps to: CC `utils/swarm/constants.ts#getSwarmSocketName`.
pub fn get_swarm_socket_name() -> String {
    format!("claude-swarm-{}", std::process::id())
}
