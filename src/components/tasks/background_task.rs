//! Maps to: CC `components/tasks/BackgroundTask.tsx:1-146`.

use super::shell_progress::{ShellProgress, TaskStatusText};
use super::task_status_utils::TaskStatus;
use crate::utils::theme::{Theme, ThemeColorKey};
use iocraft::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundTaskData {
    LocalBash {
        command: String,
        monitor_description: Option<String>,
        status: TaskStatus,
    },
    LocalAgent {
        description: String,
        status: TaskStatus,
        notified: bool,
    },
    InProcessTeammate {
        agent_name: String,
        color: ThemeColorKey,
        activity: String,
    },
    LocalWorkflow {
        name: String,
        agent_count: usize,
        status: TaskStatus,
        notified: bool,
    },
    MonitorMcp {
        description: String,
        status: TaskStatus,
        notified: bool,
    },
    Dream {
        description: String,
        phase: String,
        files_touched: usize,
        sessions_reviewing: usize,
        status: TaskStatus,
        notified: bool,
    },
}

fn truncate_activity(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_string()
    } else if limit > 1 {
        format!("{}…", value.chars().take(limit - 1).collect::<String>())
    } else {
        "…".to_string()
    }
}

#[derive(Default, Props)]
pub struct BackgroundTaskProps {
    pub task: Option<BackgroundTaskData>,
    pub max_activity_width: Option<usize>,
}

#[component]
pub fn BackgroundTask(props: &BackgroundTaskProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let Some(task) = props.task.as_ref() else {
        return element! { Fragment }.into_any();
    };
    let limit = props.max_activity_width.unwrap_or(40);
    let theme = hooks.use_context::<Theme>();
    match task {
        BackgroundTaskData::LocalBash { command, monitor_description, status } => element! { View(flex_direction: FlexDirection::Row) {
            Text(content: format!("{} ", truncate_activity(monitor_description.as_deref().unwrap_or(command), limit)))
            ShellProgress(status: *status)
        }}.into_any(),
        BackgroundTaskData::LocalAgent { description, status, notified } => element! { View(flex_direction: FlexDirection::Row) {
            Text(content: format!("{} ", truncate_activity(description, limit)))
            TaskStatusText(status: *status, label: (*status == TaskStatus::Completed).then(|| "done".to_string()), suffix: (*status == TaskStatus::Completed && !*notified).then(|| ", unread".to_string()))
        }}.into_any(),
        BackgroundTaskData::InProcessTeammate { agent_name, color, activity } => element! { View(flex_direction: FlexDirection::Row) {
            Text(content: format!("@{agent_name}"), color: theme.color(*color))
            Text(content: format!(": {}", truncate_activity(activity, limit)), dim: true)
        }}.into_any(),
        BackgroundTaskData::LocalWorkflow { name, agent_count, status, notified } => {
            let label = if *status == TaskStatus::Running { Some(format!("{agent_count} {}", if *agent_count == 1 { "agent" } else { "agents" })) } else if *status == TaskStatus::Completed { Some("done".to_string()) } else { None };
            element! { View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{} ", truncate_activity(name, limit)))
                TaskStatusText(status: *status, label: label, suffix: (*status == TaskStatus::Completed && !*notified).then(|| ", unread".to_string()))
            }}.into_any()
        }
        BackgroundTaskData::MonitorMcp { description, status, notified } => element! { View(flex_direction: FlexDirection::Row) {
            Text(content: format!("{} ", truncate_activity(description, limit)))
            TaskStatusText(status: *status, label: (*status == TaskStatus::Completed).then(|| "done".to_string()), suffix: (*status == TaskStatus::Completed && !*notified).then(|| ", unread".to_string()))
        }}.into_any(),
        BackgroundTaskData::Dream { description, phase, files_touched, sessions_reviewing, status, notified } => {
            let (count, noun) = if phase == "updating" && *files_touched > 0 { (*files_touched, "file") } else { (*sessions_reviewing, "session") };
            element! { View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{description} "))
                Text(content: format!("· {phase} · {count} {} ", if count == 1 { noun.to_string() } else { format!("{noun}s") }), dim: true)
                TaskStatusText(status: *status, label: (*status == TaskStatus::Completed).then(|| "done".to_string()), suffix: (*status == TaskStatus::Completed && !*notified).then(|| ", unread".to_string()))
            }}.into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_agent_and_dream_rows_preserve_done_unread_and_plural_details() {
        let theme = *crate::utils::theme::current();
        let local = element! { ContextProvider(value: Context::owned(theme)) { BackgroundTask(task: Some(BackgroundTaskData::LocalAgent { description: "review code".to_string(), status: TaskStatus::Completed, notified: false })) } }.render(Some(80)).to_string();
        assert!(local.contains("review code (done, unread)"));
        let dream = element! { ContextProvider(value: Context::owned(theme)) { BackgroundTask(task: Some(BackgroundTaskData::Dream { description: "Dream".to_string(), phase: "updating".to_string(), files_touched: 2, sessions_reviewing: 5, status: TaskStatus::Running, notified: true })) } }.render(Some(80)).to_string();
        assert!(dream.contains("Dream · updating · 2 files (running)"));
    }
}
