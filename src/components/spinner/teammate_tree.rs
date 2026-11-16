//! Teammate snapshot/projection types.
//!
//! @nyan: the rendered `TeammateSpinnerTree` was removed in TUI slice E. The
//! agent panel (docs/nyan/tui.md §6) owns teammate rows now, and this module
//! keeps the data shapes it still needs: the `AppState.tasks` teammate
//! snapshot, its running-only projection, and the message-preview helpers.
//! Maps to CC `components/Spinner/TeammateSpinnerTree.tsx` /
//! `TeammateSpinnerLine.tsx` for the data half only.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const TEAMMATE_SELECT_HINT: &str = "shift + ↑/↓ to select";

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TeammateSpinnerColor {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    #[default]
    Cyan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateRecentActivity {
    pub activity_description: Option<String>,
    pub is_search: bool,
    pub is_read: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateSpinnerTask {
    pub id: String,
    pub agent_name: String,
    pub color: TeammateSpinnerColor,
    pub activity: Option<String>,
    /// Explicit spinner verb used when this teammate is foregrounded. If absent,
    /// the main spinner can fall back to recent activity or the leader verb.
    pub spinner_verb: Option<String>,
    pub recent_activities: Vec<TeammateRecentActivity>,
    pub is_idle: bool,
    pub idle_text: Option<String>,
    pub past_tense_status: Option<String>,
    pub shutdown_requested: bool,
    pub awaiting_plan_approval: bool,
    pub tool_use_count: usize,
    pub token_count: usize,
    /// Explicit frozen work duration used by the foregrounded-idle spinner row.
    pub worked_for_ms: Option<u64>,
    pub preview_lines: Vec<String>,
}

pub const IN_PROCESS_TEAMMATE_TASK_TYPE: &str = "in_process_teammate";
pub const RUNNING_TASK_STATUS: &str = "running";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeammateMessageBlockSnapshot {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        description: Option<String>,
        prompt: Option<String>,
        command: Option<String>,
        query: Option<String>,
        pattern: Option<String>,
    },
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeammateMessageSnapshot {
    pub message_type: String,
    pub blocks: Vec<TeammateMessageBlockSnapshot>,
}

/// Spinner/UI snapshot of an in-process teammate task.
/// Stored in `AppState.tasks` as [`crate::state::app_state_store::TaskState::InProcessTeammate`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeammateTaskSnapshot {
    pub id: String,
    pub task_type: String,
    pub status: String,
    /// Maps to: CC `TaskStateBase.notified` — projected from the in-process
    /// teammate registry so `evict_terminal_task` can apply CC's guard
    /// (utils/task/framework.ts:133). P4 (2026-08-02).
    pub notified: bool,
    pub agent_name: String,
    pub color: Option<String>,
    pub is_idle: bool,
    pub idle_text: Option<String>,
    pub past_tense_status: Option<String>,
    pub shutdown_requested: bool,
    pub awaiting_plan_approval: bool,
    pub tool_use_count: usize,
    pub token_count: usize,
    /// Frozen work duration from the readonly task snapshot. This substitutes
    /// for official `Date.now() - startTime - totalPausedMs` without a live task.
    pub worked_for_ms: Option<u64>,
    pub last_activity_description: Option<String>,
    /// Official `spinnerVerb` snapshot for the foregrounded teammate row.
    pub spinner_verb: Option<String>,
    pub recent_activities: Vec<TeammateRecentActivity>,
    pub messages: Vec<TeammateMessageSnapshot>,
}

/// Pure counterpart of official `getRunningTeammatesSorted(tasks)` plus the
/// `TeammateSpinnerLine` prop projection. It accepts an explicit task snapshot
/// instead of reading AppState, so the main-screen renderer keeps the task-store
/// seam without starting teammate execution or mutating runtime state.
pub fn running_teammate_spinner_tasks_from_snapshots(
    snapshots: &[TeammateTaskSnapshot],
) -> Vec<TeammateSpinnerTask> {
    let mut running = snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.task_type == IN_PROCESS_TEAMMATE_TASK_TYPE
                && snapshot.status == RUNNING_TASK_STATUS
        })
        .collect::<Vec<_>>();
    running.sort_by(|a, b| a.agent_name.cmp(&b.agent_name));

    running
        .into_iter()
        .map(|snapshot| TeammateSpinnerTask {
            id: snapshot.id.clone(),
            agent_name: snapshot.agent_name.clone(),
            color: parse_teammate_spinner_color(snapshot.color.as_deref()),
            activity: snapshot.last_activity_description.clone(),
            spinner_verb: snapshot.spinner_verb.clone(),
            recent_activities: snapshot.recent_activities.clone(),
            is_idle: snapshot.is_idle,
            idle_text: snapshot.idle_text.clone(),
            past_tense_status: snapshot.past_tense_status.clone(),
            shutdown_requested: snapshot.shutdown_requested,
            awaiting_plan_approval: snapshot.awaiting_plan_approval,
            tool_use_count: snapshot.tool_use_count,
            token_count: snapshot.token_count,
            worked_for_ms: snapshot.worked_for_ms,
            preview_lines: teammate_message_preview_lines(&snapshot.messages),
        })
        .collect()
}

/// Maps to: CC `getAllInProcessTeammateTasks(tasks)` filtered to running,
/// then projected for `TeammateSpinnerTree`.
pub fn running_teammate_spinner_tasks_from_app_tasks(
    tasks: &std::collections::BTreeMap<
        String,
        std::sync::Arc<crate::state::app_state_store::TaskState>,
    >,
) -> Vec<TeammateSpinnerTask> {
    let snapshots = tasks
        .values()
        .filter_map(|task| task.as_in_process_teammate())
        .cloned()
        .collect::<Vec<_>>();
    running_teammate_spinner_tasks_from_snapshots(&snapshots)
}

/// @nyan: shared with the agent panel (docs/nyan/tui.md §6.1) so teammate
/// rows use the same `agent_*` palette as the rest of the teammate UI.
pub fn parse_teammate_spinner_color(value: Option<&str>) -> TeammateSpinnerColor {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("red") => TeammateSpinnerColor::Red,
        Some("blue") => TeammateSpinnerColor::Blue,
        Some("green") => TeammateSpinnerColor::Green,
        Some("yellow") => TeammateSpinnerColor::Yellow,
        Some("purple") => TeammateSpinnerColor::Purple,
        Some("orange") => TeammateSpinnerColor::Orange,
        Some("pink") => TeammateSpinnerColor::Pink,
        Some("cyan") => TeammateSpinnerColor::Cyan,
        _ => TeammateSpinnerColor::Cyan,
    }
}

/// Pure counterpart of official `TeammateSpinnerLine.tsx#getMessagePreview`.
pub fn teammate_message_preview_lines(messages: &[TeammateMessageSnapshot]) -> Vec<String> {
    let mut lines = Vec::new();

    for message in messages.iter().rev() {
        if lines.len() >= 3 {
            break;
        }
        if message.message_type != "user" && message.message_type != "assistant" {
            continue;
        }

        for block in &message.blocks {
            if lines.len() >= 3 {
                break;
            }
            match block {
                TeammateMessageBlockSnapshot::ToolUse {
                    name,
                    description,
                    prompt,
                    command,
                    query,
                    pattern,
                } => {
                    let fallback = format!("Using {name}…");
                    let tool_line = [
                        description.as_deref(),
                        prompt.as_deref(),
                        command.as_deref(),
                        query.as_deref(),
                        pattern.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .find(|text| !text.trim().is_empty())
                    .and_then(|text| text.lines().next())
                    .unwrap_or(fallback.as_str());
                    lines.push(truncate_to_width(tool_line, 80));
                }
                TeammateMessageBlockSnapshot::Text { text } => {
                    let text_lines = text
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .collect::<Vec<_>>();
                    for line in text_lines.iter().rev() {
                        if lines.len() >= 3 {
                            break;
                        }
                        lines.push(truncate_to_width(line, 80));
                    }
                }
                TeammateMessageBlockSnapshot::Other => {}
            }
        }
    }

    lines.reverse();
    lines
}

impl TeammateSpinnerColor {
    /// Theme key for consumers that carry colors instead of rendering them
    /// directly (the agent panel's row projection).
    pub fn theme_key(self) -> crate::utils::theme::ThemeColorKey {
        use crate::utils::theme::ThemeColorKey as Key;
        match self {
            TeammateSpinnerColor::Red => Key::AgentRed,
            TeammateSpinnerColor::Blue => Key::AgentBlue,
            TeammateSpinnerColor::Green => Key::AgentGreen,
            TeammateSpinnerColor::Yellow => Key::AgentYellow,
            TeammateSpinnerColor::Purple => Key::AgentPurple,
            TeammateSpinnerColor::Orange => Key::AgentOrange,
            TeammateSpinnerColor::Pink => Key::AgentPink,
            TeammateSpinnerColor::Cyan => Key::AgentCyan,
        }
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 || UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis_width = UnicodeWidthStr::width("…");
    if max_width <= ellipsis_width {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + ellipsis_width > max_width {
            break;
        }
        used += ch_width;
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn teammate_snapshot(id: &str, name: &str, status: &str) -> TeammateTaskSnapshot {
        TeammateTaskSnapshot {
            id: id.to_string(),
            task_type: IN_PROCESS_TEAMMATE_TASK_TYPE.to_string(),
            status: status.to_string(),
            agent_name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn running_teammate_spinner_tasks_filter_sort_and_project_official_task_store_shape() {
        let mut zebra = teammate_snapshot("z", "zebra", RUNNING_TASK_STATUS);
        zebra.color = Some("purple".to_string());
        zebra.tool_use_count = 3;
        zebra.token_count = 12_345;
        zebra.last_activity_description = Some("Reviewing patch".to_string());
        zebra.spinner_verb = Some("Reviewing teammate work".to_string());
        zebra.recent_activities = vec![TeammateRecentActivity {
            activity_description: Some("Reading files".to_string()),
            is_read: true,
            ..Default::default()
        }];
        let mut alpha = teammate_snapshot("a", "alpha", RUNNING_TASK_STATUS);
        alpha.color = Some("orange".to_string());
        alpha.awaiting_plan_approval = true;
        alpha.messages = vec![TeammateMessageSnapshot {
            message_type: "assistant".to_string(),
            blocks: vec![TeammateMessageBlockSnapshot::Text {
                text: "First\nSecond".to_string(),
            }],
        }];
        let stopped = teammate_snapshot("s", "stopped", "completed");
        let non_teammate = TeammateTaskSnapshot {
            id: "other".to_string(),
            task_type: "local_agent".to_string(),
            status: RUNNING_TASK_STATUS.to_string(),
            agent_name: "other".to_string(),
            ..Default::default()
        };

        let tasks =
            running_teammate_spinner_tasks_from_snapshots(&[zebra, stopped, alpha, non_teammate]);

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].agent_name, "alpha");
        assert_eq!(tasks[0].color, TeammateSpinnerColor::Orange);
        assert!(tasks[0].awaiting_plan_approval);
        assert_eq!(tasks[0].preview_lines, vec!["First", "Second"]);
        assert_eq!(tasks[1].agent_name, "zebra");
        assert_eq!(tasks[1].color, TeammateSpinnerColor::Purple);
        assert_eq!(tasks[1].tool_use_count, 3);
        assert_eq!(tasks[1].token_count, 12_345);
        assert_eq!(tasks[1].activity.as_deref(), Some("Reviewing patch"));
        assert_eq!(
            tasks[1].spinner_verb.as_deref(),
            Some("Reviewing teammate work")
        );
        assert_eq!(tasks[1].recent_activities.len(), 1);
    }

    #[test]
    fn teammate_message_preview_lines_match_official_recent_message_rules() {
        let messages = vec![
            TeammateMessageSnapshot {
                message_type: "system".to_string(),
                blocks: vec![TeammateMessageBlockSnapshot::Text {
                    text: "Ignored".to_string(),
                }],
            },
            TeammateMessageSnapshot {
                message_type: "user".to_string(),
                blocks: vec![TeammateMessageBlockSnapshot::Text {
                    text: "older line".to_string(),
                }],
            },
            TeammateMessageSnapshot {
                message_type: "assistant".to_string(),
                blocks: vec![
                    TeammateMessageBlockSnapshot::ToolUse {
                        name: "Bash".to_string(),
                        description: None,
                        prompt: None,
                        command: Some("cargo test\n--nocapture".to_string()),
                        query: None,
                        pattern: None,
                    },
                    TeammateMessageBlockSnapshot::Text {
                        text: "\nnewer one\nnewer two".to_string(),
                    },
                ],
            },
        ];

        assert_eq!(
            teammate_message_preview_lines(&messages),
            vec!["newer one", "newer two", "cargo test"]
        );
    }

    #[test]
    fn teammate_message_preview_lines_use_tool_fallback_and_cap_to_three() {
        let messages = vec![TeammateMessageSnapshot {
            message_type: "assistant".to_string(),
            blocks: vec![
                TeammateMessageBlockSnapshot::ToolUse {
                    name: "Read".to_string(),
                    description: None,
                    prompt: None,
                    command: None,
                    query: None,
                    pattern: None,
                },
                TeammateMessageBlockSnapshot::Text {
                    text: "one\ntwo\nthree\nfour".to_string(),
                },
            ],
        }];

        assert_eq!(
            teammate_message_preview_lines(&messages),
            vec!["three", "four", "Using Read…"]
        );
    }
}
