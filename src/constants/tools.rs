//! Maps to: CC `constants/tools.ts:36-46` and `:104-112`.
use std::{collections::HashSet, sync::LazyLock};
/// Runtime set mapped through the established build-audience L1 contract.
/// WORKFLOW_SCRIPTS is unavailable in this port; its tool is never registered.
pub static ALL_AGENT_DISALLOWED_TOOLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        crate::tools::task_output_tool::constants::TASK_OUTPUT_TOOL_NAME,
        crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME,
        crate::tools::enter_plan_mode_tool::constants::ENTER_PLAN_MODE_TOOL_NAME,
        crate::tools::ask_user_question_tool::prompt::ASK_USER_QUESTION_TOOL_NAME,
        crate::tools::task_stop_tool::prompt::TASK_STOP_TOOL_NAME,
    ])
});

/// Maps to: CC `constants/tools.ts:104-112` `COORDINATOR_MODE_ALLOWED_TOOLS`.
///
/// "Tools allowed in coordinator mode - only output and agent management tools
/// for the coordinator." Consumed by
/// `utils/tool_pool.rs#apply_coordinator_tool_filter`.
pub static COORDINATOR_MODE_ALLOWED_TOOLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
        crate::tools::task_stop_tool::prompt::TASK_STOP_TOOL_NAME,
        crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME,
        crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
    ])
});
