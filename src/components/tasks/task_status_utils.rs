//! Maps to: CC `components/tasks/taskStatusUtils.tsx:1-118`.

use crate::utils::theme::Theme;
use iocraft::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    #[default]
    Running,
    Completed,
    Failed,
    Killed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskStatusOptions {
    pub is_idle: bool,
    pub awaiting_approval: bool,
    pub has_error: bool,
    pub shutdown_requested: bool,
}

pub fn is_terminal_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
    )
}

pub fn get_task_status_icon(status: TaskStatus, options: TaskStatusOptions) -> &'static str {
    let figures = crate::constants::figures::get();
    if options.has_error {
        figures.cross
    } else if options.awaiting_approval {
        figures.question_mark_prefix
    } else if options.shutdown_requested {
        figures.warning
    } else {
        match status {
            TaskStatus::Running if options.is_idle => figures.ellipsis,
            TaskStatus::Running => figures.play,
            TaskStatus::Completed => figures.tick,
            TaskStatus::Failed | TaskStatus::Killed => figures.cross,
            TaskStatus::Pending => figures.bullet,
        }
    }
}

pub fn get_task_status_color(
    theme: &Theme,
    status: TaskStatus,
    options: TaskStatusOptions,
) -> Color {
    if options.has_error || status == TaskStatus::Failed {
        theme.error
    } else if options.awaiting_approval
        || options.shutdown_requested
        || status == TaskStatus::Killed
    {
        theme.warning
    } else if options.is_idle || matches!(status, TaskStatus::Pending | TaskStatus::Running) {
        theme.background
    } else {
        theme.success
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentActivity {
    pub activity_description: Option<String>,
    pub is_search: bool,
    pub is_read: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateActivityProjection {
    pub shutdown_requested: bool,
    pub awaiting_plan_approval: bool,
    pub is_idle: bool,
    pub recent_activities: Vec<RecentActivity>,
    pub last_activity_description: Option<String>,
}

/// CC's `summarizeRecentActivities` takes the structural
/// `{activityDescription?, isSearch?, isRead?}`; this is that shape for the
/// task-status row type.
impl crate::utils::collapse_read_search::RecentActivity for RecentActivity {
    fn is_search(&self) -> bool {
        self.is_search
    }

    fn is_read(&self) -> bool {
        self.is_read
    }

    fn activity_description(&self) -> Option<String> {
        self.activity_description.clone()
    }
}

/// Maps to: CC `components/tasks/taskStatusUtils.tsx:11` importing
/// `summarizeRecentActivities` from `utils/collapseReadSearch.js` and calling
/// it at `:86-87`. This module previously carried a private copy that inlined
/// its own three-arm summary text — the third such copy in the port, after
/// `teammate_tree.rs`'s (removed in the same batch). Behaviour is unchanged
/// for the reachable cases; the shared helper additionally covers the
/// memory/list/REPL segments and past tense CC's text has.
fn recent_activity_summary(activities: &[RecentActivity]) -> Option<String> {
    crate::utils::collapse_read_search::summarize_recent_activities(activities)
}

pub fn describe_teammate_activity(task: &TeammateActivityProjection) -> String {
    if task.shutdown_requested {
        "stopping".to_string()
    } else if task.awaiting_plan_approval {
        "awaiting approval".to_string()
    } else if task.is_idle {
        "idle".to_string()
    } else {
        recent_activity_summary(&task.recent_activities)
            .or_else(|| task.last_activity_description.clone())
            .unwrap_or_else(|| "working".to_string())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FooterTaskProjection {
    pub is_background: bool,
    pub is_panel_agent: bool,
    pub is_in_process_teammate: bool,
}

pub fn should_hide_tasks_footer(tasks: &[FooterTaskProjection], show_spinner_tree: bool) -> bool {
    if !show_spinner_tree {
        return false;
    }
    let visible = tasks
        .iter()
        .filter(|task| task.is_background && !task.is_panel_agent)
        .collect::<Vec<_>>();
    !visible.is_empty() && visible.iter().all(|task| task.is_in_process_teammate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_icon_and_color_precedence_match_official() {
        let theme = *crate::utils::theme::current();
        assert!(is_terminal_status(TaskStatus::Killed));
        assert!(!is_terminal_status(TaskStatus::Pending));
        assert_eq!(
            get_task_status_icon(
                TaskStatus::Running,
                TaskStatusOptions {
                    awaiting_approval: true,
                    ..Default::default()
                }
            ),
            crate::constants::figures::get().question_mark_prefix
        );
        assert_eq!(
            get_task_status_color(&theme, TaskStatus::Completed, TaskStatusOptions::default()),
            theme.success
        );
        assert_eq!(
            get_task_status_color(
                &theme,
                TaskStatus::Completed,
                TaskStatusOptions {
                    has_error: true,
                    ..Default::default()
                }
            ),
            theme.error
        );
    }

    #[test]
    fn teammate_activity_fallback_order_matches_official() {
        assert_eq!(
            describe_teammate_activity(&TeammateActivityProjection {
                shutdown_requested: true,
                ..Default::default()
            }),
            "stopping"
        );
        assert_eq!(
            describe_teammate_activity(&TeammateActivityProjection {
                awaiting_plan_approval: true,
                ..Default::default()
            }),
            "awaiting approval"
        );
        assert_eq!(
            describe_teammate_activity(&TeammateActivityProjection {
                is_idle: true,
                ..Default::default()
            }),
            "idle"
        );
        assert_eq!(
            describe_teammate_activity(&TeammateActivityProjection {
                last_activity_description: Some("editing".to_string()),
                ..Default::default()
            }),
            "editing"
        );
    }

    #[test]
    fn footer_hides_only_non_empty_all_teammate_set_in_spinner_mode() {
        let teammate = FooterTaskProjection {
            is_background: true,
            is_in_process_teammate: true,
            ..Default::default()
        };
        assert!(should_hide_tasks_footer(
            std::slice::from_ref(&teammate),
            true
        ));
        assert!(!should_hide_tasks_footer(&[teammate], false));
        assert!(!should_hide_tasks_footer(&[], true));
        assert!(!should_hide_tasks_footer(
            &[FooterTaskProjection {
                is_background: true,
                ..Default::default()
            }],
            true
        ));
    }
}
