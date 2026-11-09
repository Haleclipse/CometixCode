//! Maps to: CC `components/messages/`.
//! Each user-visible transcript block lives in its own component module, as in
//! Claude Code's React tree. `Message.tsx`-style dispatch stays outside this
//! directory.

pub mod advisor_message;
pub mod assistant_redacted_thinking_message;
pub mod assistant_text_message;
pub mod assistant_thinking_message;
pub mod assistant_tool_use_message;
pub mod attachment_message;
pub mod collapsed_read_search_content;
pub mod compact_boundary_message;
pub mod grouped_tool_use_content;
pub mod highlighted_thinking_text;
pub mod hook_progress_message;
pub mod null_rendering_attachments;
pub mod plan_approval_message;
pub mod shutdown_message;
pub mod system_api_error_message;
pub mod system_text_message;
pub mod task_assignment_message;
pub mod team_mem_collapsed;
pub mod team_mem_saved;
pub mod turn_duration_message;
pub mod user_agent_notification_message;
pub mod user_bash_input_message;
pub mod user_bash_output_message;
pub mod user_channel_message;
pub mod user_command_message;
pub mod user_image_message;
pub mod user_local_command_output_message;
pub mod user_memory_input_message;
pub mod user_plan_message;
pub mod user_prompt_message;
pub mod user_resource_update_message;
pub mod user_teammate_message;
pub mod user_text_message;
pub mod user_tool_result_message;

pub use advisor_message::AdvisorMessage;
pub use assistant_redacted_thinking_message::AssistantRedactedThinkingMessage;
pub use assistant_text_message::{AssistantTextMessage, StreamingAssistantTextMessage};
pub use assistant_thinking_message::AssistantThinkingMessage;
pub use assistant_tool_use_message::{AssistantToolUseMessage, derive_tool_use_status};
pub use attachment_message::AttachmentMessage;
pub use collapsed_read_search_content::CollapsedReadSearchContent;
pub use compact_boundary_message::CompactBoundaryMessage;
pub use grouped_tool_use_content::GroupedToolUseContent;
pub use hook_progress_message::HookProgressMessage;
pub use plan_approval_message::PlanApprovalMessage;
pub use shutdown_message::ShutdownMessage;
pub use system_api_error_message::SystemApiErrorMessage;
pub use system_text_message::{StopHookSummaryMessage, SystemTextMessage};
pub use task_assignment_message::TaskAssignmentMessage;
pub use turn_duration_message::TurnDurationMessage;
pub use user_agent_notification_message::UserAgentNotificationMessage;
pub use user_bash_input_message::UserBashInputMessage;
pub use user_bash_output_message::UserBashOutputMessage;
pub use user_channel_message::UserChannelMessage;
pub use user_command_message::UserCommandMessage;
pub use user_image_message::UserImageMessage;
pub use user_local_command_output_message::UserLocalCommandOutputMessage;
pub use user_memory_input_message::UserMemoryInputMessage;
pub use user_plan_message::UserPlanMessage;
pub use user_resource_update_message::UserResourceUpdateMessage;
pub use user_teammate_message::UserTeammateMessage;
pub use user_text_message::UserTextMessage;
pub use user_tool_result_message::UserToolResultMessage;
