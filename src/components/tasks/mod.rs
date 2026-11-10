//! Maps to: CC `components/tasks/**`.

pub mod async_agent_detail_dialog;
pub mod background_task;
pub mod background_task_status;
pub mod background_tasks_dialog;
pub mod dream_detail_dialog;
pub mod in_process_teammate_detail_dialog;
pub mod render_tool_activity;
pub mod shell_detail_dialog;
pub mod shell_progress;
pub mod task_status_utils;

pub use async_agent_detail_dialog::{AsyncAgentDetailData, AsyncAgentDetailDialog};
pub use background_task::{BackgroundTask, BackgroundTaskData};
pub use background_task_status::{
    BackgroundTaskStatus, BackgroundTaskStatusData, TeammatePillData,
};
pub use background_tasks_dialog::{
    BackgroundTaskCategory, BackgroundTaskDetailData, BackgroundTasksDialog,
    BackgroundTasksDialogAction, BackgroundTasksDialogItem,
};
pub use dream_detail_dialog::{DreamDetailData, DreamDetailDialog, DreamTurn};
pub use in_process_teammate_detail_dialog::{InProcessTeammateDetailDialog, TeammateDetailData};
pub use render_tool_activity::{ToolActivity, render_tool_activity};
pub use shell_detail_dialog::{ShellDetailData, ShellDetailDialog, TaskOutputResult};
pub use shell_progress::{ShellProgress, TaskStatusText};
pub use task_status_utils::{
    TaskStatus, describe_teammate_activity, get_task_status_color, get_task_status_icon,
    is_terminal_status, should_hide_tasks_footer,
};
