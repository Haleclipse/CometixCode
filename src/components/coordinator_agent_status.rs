//! Maps to: CC `components/CoordinatorAgentStatus.tsx`.
//!
//! AppState selectors, click handlers, and eviction timers stay outside this
//! render slice. This module preserves official visible-task filtering, count
//! math, and row rendering from a caller-provided coordinator-task snapshot.

use crate::constants::figures::{BLACK_CIRCLE, PAUSE_ICON, PLAY_ICON};
use crate::utils::format::{format_duration, format_number};
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordinatorTaskStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Killed,
}

impl CoordinatorTaskStatus {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        }
    }
}

/// Maps to: CC `components/tasks/taskStatusUtils.tsx#isTerminalStatus`.
pub fn coordinator_is_terminal_status(status: CoordinatorTaskStatus) -> bool {
    matches!(
        status,
        CoordinatorTaskStatus::Completed
            | CoordinatorTaskStatus::Failed
            | CoordinatorTaskStatus::Killed
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorAgentProgress {
    pub token_count: Option<u64>,
    pub last_activity: Option<String>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorAgentTask {
    pub id: String,
    pub status: CoordinatorTaskStatus,
    pub start_time_ms: u64,
    pub end_time_ms: Option<u64>,
    pub total_paused_ms: Option<u64>,
    pub description: String,
    pub progress: Option<CoordinatorAgentProgress>,
    pub pending_message_count: usize,
    /// Maps to CC `LocalAgentTaskState.evictAfter`; `Some(0)` means immediate
    /// dismiss and should not be visible.
    pub evict_after_ms: Option<u64>,
}

/// Maps to: CC `CoordinatorAgentStatus.tsx#getVisibleAgentTasks`.
pub fn get_visible_agent_tasks(tasks: &[CoordinatorAgentTask]) -> Vec<CoordinatorAgentTask> {
    let mut visible = tasks
        .iter()
        .filter(|task| task.evict_after_ms != Some(0))
        .cloned()
        .collect::<Vec<_>>();
    visible.sort_by_key(|task| task.start_time_ms);
    visible
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorMainLineDisplay {
    pub prefix: String,
    pub bullet: String,
    pub dim: bool,
    pub bold: bool,
    pub text: String,
}

/// Maps to: CC `CoordinatorAgentStatus.tsx#MainLine` display derivation.
pub fn coordinator_main_line_display(
    is_selected: bool,
    is_viewed: bool,
    hover: bool,
) -> CoordinatorMainLineDisplay {
    let fig = crate::constants::figures::get();
    CoordinatorMainLineDisplay {
        prefix: if is_selected || hover {
            format!("{} ", fig.pointer)
        } else {
            "  ".to_string()
        },
        bullet: if is_viewed {
            BLACK_CIRCLE.to_string()
        } else {
            fig.circle.to_string()
        },
        dim: !is_selected && !is_viewed && !hover,
        bold: is_viewed,
        text: "main".to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinatorAgentLineDisplay {
    pub prefix: String,
    pub bullet: String,
    pub name: Option<String>,
    pub description: String,
    pub sep: String,
    pub elapsed: String,
    pub token_text: String,
    pub queued_text: String,
    pub hint_text: String,
    pub dim: bool,
    pub bold: bool,
    pub running: bool,
}

/// Maps to: CC `CoordinatorAgentStatus.tsx#AgentLine` display derivation.
pub fn coordinator_agent_line_display(
    task: &CoordinatorAgentTask,
    name: Option<&str>,
    is_selected: bool,
    is_viewed: bool,
    hover: bool,
    columns: usize,
    now_ms: u64,
) -> CoordinatorAgentLineDisplay {
    let fig = crate::constants::figures::get();
    let is_running = !coordinator_is_terminal_status(task.status);
    let paused_ms = task.total_paused_ms.unwrap_or(0);
    let elapsed_ms = if is_running {
        now_ms
            .saturating_sub(task.start_time_ms)
            .saturating_sub(paused_ms)
    } else {
        task.end_time_ms
            .unwrap_or(task.start_time_ms)
            .saturating_sub(task.start_time_ms)
            .saturating_sub(paused_ms)
    };
    let elapsed = format_duration(elapsed_ms);
    let token_text = task
        .progress
        .as_ref()
        .and_then(|progress| progress.token_count)
        .filter(|tokens| *tokens > 0)
        .map(|tokens| {
            let arrow = if task
                .progress
                .as_ref()
                .and_then(|progress| progress.last_activity.as_ref())
                .is_some()
            {
                fig.arrow_down
            } else {
                fig.arrow_up
            };
            format!(" · {} {} tokens", arrow, format_number(tokens))
        })
        .unwrap_or_default();
    let queued_text = if task.pending_message_count > 0 {
        format!(" · {} queued", task.pending_message_count)
    } else {
        String::new()
    };
    let display_description = task
        .progress
        .as_ref()
        .and_then(|progress| progress.summary.as_deref())
        .unwrap_or(&task.description);
    let highlighted = is_selected || hover;
    let prefix = if highlighted {
        format!("{} ", fig.pointer)
    } else {
        "  ".to_string()
    };
    let bullet = if is_viewed { BLACK_CIRCLE } else { fig.circle };
    let sep = if is_running { PLAY_ICON } else { PAUSE_ICON };
    let name_part = name.map(|name| format!("{name}: ")).unwrap_or_default();
    let hint_text = if is_selected && !is_viewed {
        format!(" · x to {}", if is_running { "stop" } else { "clear" })
    } else {
        String::new()
    };
    let suffix_part = format!(" {sep} {elapsed}{token_text}{queued_text}{hint_text}");
    let used = UnicodeWidthStr::width(prefix.as_str())
        + UnicodeWidthStr::width(format!("{bullet} ").as_str())
        + UnicodeWidthStr::width(name_part.as_str())
        + UnicodeWidthStr::width(suffix_part.as_str());
    let available = columns.saturating_sub(used);
    let description = truncate_to_width(display_description, available);

    CoordinatorAgentLineDisplay {
        prefix,
        bullet: bullet.to_string(),
        name: name.map(ToOwned::to_owned),
        description,
        sep: sep.to_string(),
        elapsed,
        token_text,
        queued_text,
        hint_text,
        dim: !highlighted && !is_viewed,
        bold: is_viewed,
        running: is_running,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoordinatorAgentStatusData {
    pub tasks: Vec<CoordinatorAgentTask>,
    pub viewing_agent_task_id: Option<String>,
    pub selected_index: Option<usize>,
    pub agent_names: Vec<(String, String)>,
    pub columns: usize,
    pub now_ms: u64,
}

#[derive(Default, Props)]
pub struct CoordinatorTaskPanelProps {
    pub data: CoordinatorAgentStatusData,
}

/// Maps to: CC `components/CoordinatorAgentStatus.tsx#CoordinatorTaskPanel`.
#[component]
pub fn CoordinatorTaskPanel(props: &CoordinatorTaskPanelProps) -> impl Into<AnyElement<'static>> {
    let data = props.data.clone();
    let visible_tasks = get_visible_agent_tasks(&data.tasks);
    if visible_tasks.is_empty() {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    let name_by_agent_id = data
        .agent_names
        .iter()
        .map(|(name, id)| (id.clone(), name.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let main = coordinator_main_line_display(
        data.selected_index == Some(0),
        data.viewing_agent_task_id.is_none(),
        false,
    );

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{}{} {}", main.prefix, main.bullet, main.text), dim: main.dim, weight: if main.bold { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
            }
            #(visible_tasks.into_iter().enumerate().map(|(index, task)| {
                let line = coordinator_agent_line_display(
                    &task,
                    name_by_agent_id.get(&task.id).map(String::as_str),
                    data.selected_index == Some(index + 1),
                    data.viewing_agent_task_id.as_deref() == Some(task.id.as_str()),
                    false,
                    data.columns,
                    data.now_ms,
                );
                element! {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: line.prefix, dim: line.dim, weight: if line.bold { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
                        Text(content: format!("{} ", line.bullet), dim: line.dim, weight: if line.bold { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
                        #(line.name.as_ref().map(|name| element! {
                            Fragment {
                                Text(content: name.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                                Text(content: ": ".to_string(), dim: line.dim, weight: if line.bold { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
                            }
                        }))
                        Text(content: format!("{} {} {}{}{}{}", line.description, line.sep, line.elapsed, line.token_text, line.queued_text, line.hint_text), dim: line.dim, weight: if line.bold { Weight::Bold } else { Weight::Normal }, wrap: TextWrap::NoWrap)
                    }
                }
            }).collect::<Vec<_>>())
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, start: u64) -> CoordinatorAgentTask {
        CoordinatorAgentTask {
            id: id.to_string(),
            status: CoordinatorTaskStatus::Running,
            start_time_ms: start,
            description: format!("working {id}"),
            ..CoordinatorAgentTask::default()
        }
    }

    #[test]
    fn coordinator_visible_tasks_filter_and_sort_like_official() {
        let tasks = vec![
            CoordinatorAgentTask {
                evict_after_ms: Some(0),
                ..task("hidden", 30)
            },
            task("b", 20),
            task("a", 10),
        ];
        let visible = get_visible_agent_tasks(&tasks);
        assert_eq!(
            visible.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn coordinator_main_line_matches_official_prefix_bullet_dim_rules() {
        let line = coordinator_main_line_display(false, true, false);
        assert_eq!(line.prefix, "  ");
        assert_eq!(line.bullet, BLACK_CIRCLE);
        assert!(line.bold);
        assert!(!line.dim);
        let selected = coordinator_main_line_display(true, false, false);
        assert!(
            selected
                .prefix
                .contains(crate::constants::figures::get().pointer)
        );
        assert!(!selected.dim);
    }

    #[test]
    fn coordinator_agent_line_uses_summary_elapsed_tokens_queued_and_hint() {
        let line = coordinator_agent_line_display(
            &CoordinatorAgentTask {
                status: CoordinatorTaskStatus::Running,
                start_time_ms: 1_000,
                description: "static description".to_string(),
                progress: Some(CoordinatorAgentProgress {
                    token_count: Some(12_500),
                    last_activity: Some("read".to_string()),
                    summary: Some("AI summary".to_string()),
                }),
                pending_message_count: 2,
                ..task("agent-1", 1_000)
            },
            Some("research"),
            true,
            false,
            false,
            120,
            62_000,
        );
        assert_eq!(line.name.as_deref(), Some("research"));
        assert_eq!(line.description, "AI summary");
        assert_eq!(line.sep, PLAY_ICON);
        assert_eq!(line.elapsed, "1m 1s");
        assert!(line.token_text.contains("12.5k tokens"));
        assert_eq!(line.queued_text, " · 2 queued");
        assert_eq!(line.hint_text, " · x to stop");
    }

    #[test]
    fn coordinator_agent_line_completed_uses_pause_and_clear_hint() {
        let line = coordinator_agent_line_display(
            &CoordinatorAgentTask {
                status: CoordinatorTaskStatus::Completed,
                start_time_ms: 1_000,
                end_time_ms: Some(4_000),
                total_paused_ms: Some(1_000),
                description: "done".to_string(),
                ..task("agent-1", 1_000)
            },
            None,
            true,
            false,
            false,
            80,
            10_000,
        );
        assert_eq!(line.sep, PAUSE_ICON);
        assert_eq!(line.elapsed, "2s");
        assert_eq!(line.hint_text, " · x to clear");
    }

    #[test]
    fn coordinator_panel_renders_main_and_agent_rows() {
        let text = element! {
            CoordinatorTaskPanel(data: CoordinatorAgentStatusData {
                tasks: vec![task("agent-1", 1_000)],
                viewing_agent_task_id: Some("agent-1".to_string()),
                selected_index: Some(1),
                agent_names: vec![("research".to_string(), "agent-1".to_string())],
                columns: 120,
                now_ms: 61_000,
            })
        }
        .render(Some(140))
        .to_string();
        assert!(text.contains("main"), "canvas=\n{text}");
        assert!(
            text.contains("research: working agent-1"),
            "canvas=\n{text}"
        );
        assert!(text.contains(PLAY_ICON), "canvas=\n{text}");
    }
}
