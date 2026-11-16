//! @nyan: built-in agent panel (docs/nyan/tui.md §6, §7).
//!
//! Projects the running background tasks in `AppState.tasks` (teammates,
//! shells, dreams) plus the local-agent registry into one row list, then
//! paints it under the prompt input, above the footer info line. `AgentPanelView`
//! is the presentational half (pure data in, lines out); `AgentPanel` is the
//! AppState-connected half mounted in `prompt_input_footer.rs`.
//!
//! Completed rows disappear immediately and leave a 30 s `/tasks to see agents`
//! hint in the mode row. Failed and killed rows stay visible for
//! [`FAILED_RETENTION_MS`] so the ✘ outcome is readable. A row that is already
//! terminal the first time this panel sees it is not shown at all (it belongs
//! to an earlier session or turn).
//!
//! @nyan: unlike Claude Code's panel, the newest tool calls of the focused row
//! expand underneath it (`@nyan` deviation, docs/nyan/tui.md §6.2).

use crate::components::tasks::task_status_utils::{
    RecentActivity, TeammateActivityProjection, describe_teammate_activity,
};
use crate::constants::figures;
use crate::keybindings::types::ContextName;
use crate::state::app_state::{use_app_state, use_set_app_state};
use crate::state::app_state_store::{FooterItem, TaskState};
use crate::tasks::dream_task::DreamTaskState;
use crate::tasks::local_agent_task::LocalAgentTaskState;
use crate::tasks::local_shell_task::guards::LocalShellTaskState;
use crate::tools::agent_tool::agent_color_manager::get_agent_color;
use crate::utils::format::{format_duration, format_tokens};
use crate::utils::theme::{Theme, ThemeColorKey};
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

/// Rows shown at ≥ 24 terminal rows (§7).
pub const MAX_ROWS_TALL: usize = 4;
/// Rows shown at 16–23 terminal rows (§7).
pub const MAX_ROWS_MEDIUM: usize = 2;
/// Below this many terminal rows the panel hides and the footer pills return.
pub const MIN_ROWS_FOR_PANEL: usize = 16;
const TALL_ROWS: usize = 24;
/// Longest name column (§6.1).
pub const MAX_NAME_WIDTH: usize = 12;
/// A row keeps its stats only if at least this much activity room remains.
const MIN_ACTIVITY_WIDTH: usize = 8;
const ROW_GAP: usize = 2;
/// A failed or killed row lingers this long so its ✘ stays readable (§6.1).
pub const FAILED_RETENTION_MS: u64 = 30_000;
/// Completed rows vanish at once and leave this long an `/tasks to see agents`
/// hint in the mode row (§6.1).
pub const FINISHED_HINT_MS: u64 = 30_000;
const TICK_INTERVAL_MS: u64 = 1_000;
/// Latest tool calls expanded under the focused row (§6.2).
pub const MAX_EXPANDED_ACTIVITIES: usize = 3;
/// The whole panel never exceeds this many lines at ≥ 24 terminal rows:
/// 4 task rows + 3 expanded calls + the key-hint line (§7).
pub const MAX_PANEL_HEIGHT: usize = 8;
/// 16–23 terminal rows: two task rows, no expansion, one key-hint line.
const MEDIUM_PANEL_HEIGHT: usize = MAX_ROWS_MEDIUM + 1;
const UNSELECTED_PREFIX: &str = "  ";
const SELECTED_PREFIX: &str = "▶ ";
/// Expanded tool calls hang off the selected row with a `⎿` leader.
const EXPANDED_PREFIX: &str = "      ⎿ ";
const EXPANDED_CONTINUATION: &str = "        ";
const HINT_PREFIX: &str = "  ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Agent,
    Teammate,
    Shell,
    Dream,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Killed,
}

impl AgentStatus {
    fn is_running(self) -> bool {
        self == Self::Running
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub id: String,
    pub kind: AgentKind,
    pub name: String,
    /// `None` falls back to the theme's `text` color.
    pub name_color: Option<ThemeColorKey>,
    pub activity: String,
    /// Right-aligned trailing stats; empty means none.
    pub stats: String,
    pub status: AgentStatus,
    /// Tool-call descriptions, oldest first (§6.2). Empty for kinds without
    /// a tool history (shell, dream).
    pub recent_activities: Vec<String>,
}

/// The row identity the footer needs for navigation and Enter. Published to
/// AppState by [`AgentPanel`] so the footer and the panel always agree on the
/// row list (the panel's retention clock is a component-local ref).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPanelItem {
    pub id: String,
    pub kind: AgentKind,
    /// Tool-call lines the focused row would expand (capped at
    /// [`MAX_EXPANDED_ACTIVITIES`]). PromptInput's footer height budget needs
    /// it to size the panel before the panel itself renders.
    pub activity_count: usize,
}

fn parse_status(status: &str) -> AgentStatus {
    match status {
        "running" | "pending" => AgentStatus::Running,
        "completed" | "succeeded" => AgentStatus::Completed,
        "failed" => AgentStatus::Failed,
        "killed" => AgentStatus::Killed,
        _ => AgentStatus::Running,
    }
}

fn compact_duration(ms: u64) -> String {
    // §6.1 shows `2m10s`; `format_duration` spaces the units apart.
    format_duration(ms).replace(' ', "")
}

fn elapsed_ms(start_ms: u64, end_ms: Option<u64>, now_ms: u64) -> u64 {
    end_ms.unwrap_or(now_ms).saturating_sub(start_ms)
}

fn agent_stats(task: &LocalAgentTaskState, now_ms: u64) -> String {
    let (tool_uses, tokens) = task
        .progress
        .as_ref()
        .map(|progress| (progress.tool_use_count, progress.token_count))
        .unwrap_or((0, 0));
    format!(
        "{} tools · {} · {}",
        tool_uses,
        format_tokens(tokens),
        compact_duration(elapsed_ms(task.start_time_ms, task.end_time_ms, now_ms)),
    )
}

fn local_agent_activity(task: &LocalAgentTaskState) -> String {
    // §6.1: latest recorded activity, else the task description.
    task.progress
        .as_ref()
        .and_then(|progress| progress.recent_activities.last())
        .map(|activity| {
            activity
                .activity_description
                .clone()
                .unwrap_or_else(|| activity.tool_name.clone())
        })
        .filter(|activity| !activity.trim().is_empty())
        .unwrap_or_else(|| task.description.clone())
}

fn local_agent_activities(task: &LocalAgentTaskState) -> Vec<String> {
    task.progress
        .as_ref()
        .map(|progress| {
            progress
                .recent_activities
                .iter()
                .rev()
                .take(MAX_EXPANDED_ACTIVITIES)
                .rev()
                .map(|activity| {
                    activity
                        .activity_description
                        .clone()
                        .unwrap_or_else(|| activity.tool_name.clone())
                })
                .filter(|activity| !activity.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn local_agent_row(task: &LocalAgentTaskState, now_ms: u64) -> AgentRow {
    AgentRow {
        id: task.task_id.clone(),
        kind: AgentKind::Agent,
        name: task.agent_type.clone(),
        name_color: get_agent_color(&task.agent_type),
        activity: local_agent_activity(task),
        stats: agent_stats(task, now_ms),
        status: parse_status(&task.status),
        recent_activities: local_agent_activities(task),
    }
}

fn teammate_activity(
    snapshot: &crate::components::spinner::teammate_tree::TeammateTaskSnapshot,
) -> String {
    describe_teammate_activity(&TeammateActivityProjection {
        shutdown_requested: snapshot.shutdown_requested,
        awaiting_plan_approval: snapshot.awaiting_plan_approval,
        is_idle: snapshot.is_idle,
        recent_activities: snapshot
            .recent_activities
            .iter()
            .map(|activity| RecentActivity {
                activity_description: activity.activity_description.clone(),
                is_search: activity.is_search,
                is_read: activity.is_read,
            })
            .collect(),
        last_activity_description: snapshot.last_activity_description.clone(),
    })
}

fn teammate_activities(
    snapshot: &crate::components::spinner::teammate_tree::TeammateTaskSnapshot,
) -> Vec<String> {
    snapshot
        .recent_activities
        .iter()
        .rev()
        .take(MAX_EXPANDED_ACTIVITIES)
        .rev()
        .filter_map(|activity| activity.activity_description.clone())
        .filter(|activity| !activity.trim().is_empty())
        .collect()
}

fn teammate_row(
    snapshot: &crate::components::spinner::teammate_tree::TeammateTaskSnapshot,
) -> AgentRow {
    AgentRow {
        id: snapshot.id.clone(),
        kind: AgentKind::Teammate,
        name: format!("@{}", snapshot.agent_name),
        name_color: Some(
            crate::components::spinner::teammate_tree::parse_teammate_spinner_color(
                snapshot.color.as_deref(),
            )
            .theme_key(),
        ),
        activity: teammate_activity(snapshot),
        // §6.1: teammate stats have no duration.
        stats: format!(
            "{} tools · {}",
            snapshot.tool_use_count,
            format_tokens(snapshot.token_count as u64)
        ),
        status: parse_status(&snapshot.status),
        recent_activities: teammate_activities(snapshot),
    }
}

fn shell_row(task: &LocalShellTaskState, now_ms: u64) -> AgentRow {
    let monitor = task.kind.as_deref() == Some("monitor");
    AgentRow {
        id: task.id.clone(),
        kind: AgentKind::Shell,
        name: "$".to_string(),
        name_color: None,
        activity: if monitor {
            task.description.clone()
        } else {
            task.command.clone()
        },
        stats: compact_duration(elapsed_ms(
            task.start_time_ms.max(0) as u64,
            task.end_time_ms.map(|end| end.max(0) as u64),
            now_ms,
        )),
        status: parse_status(&task.status),
        // Shells have no tool-call history (§6.2).
        recent_activities: Vec::new(),
    }
}

fn dream_row(task: &DreamTaskState, now_ms: u64) -> AgentRow {
    AgentRow {
        id: task.id.clone(),
        kind: AgentKind::Dream,
        name: "dream".to_string(),
        name_color: None,
        activity: format!("{} · {}", task.phase.as_str(), task.description),
        stats: compact_duration(elapsed_ms(task.start_time, task.end_time, now_ms)),
        status: parse_status(&task.status),
        // Dreams have no tool-call history (§6.2).
        recent_activities: Vec::new(),
    }
}

/// Every panel-visible task, running or terminal, sorted: running first, then
/// newest start time, then id (determinism). Local agents come from their
/// registry — the `Other` mirror in `AppState.tasks` is skipped so the two
/// sources cannot duplicate a row.
pub fn project_agent_rows(
    tasks: &BTreeMap<String, Arc<TaskState>>,
    local_agents: &[LocalAgentTaskState],
    now_ms: u64,
) -> Vec<AgentRow> {
    // (running, start_time_ms, id, row)
    let mut sortable: Vec<(bool, u64, String, AgentRow)> = Vec::new();
    for task in tasks.values() {
        match task.as_ref() {
            TaskState::InProcessTeammate(snapshot) => {
                let row = teammate_row(snapshot);
                // Teams are always background work; there is no foreground
                // in-process teammate.
                sortable.push((row.status.is_running(), 0, row.id.clone(), row));
            }
            TaskState::LocalShell(task) => {
                // A foreground shell (`is_backgrounded == false`) is the
                // synchronous Bash tool, already shown in the message stream.
                if !task.is_backgrounded {
                    continue;
                }
                let row = shell_row(task, now_ms);
                sortable.push((
                    row.status.is_running(),
                    task.start_time_ms.max(0) as u64,
                    row.id.clone(),
                    row,
                ));
            }
            TaskState::Dream(task) => {
                let row = dream_row(task, now_ms);
                sortable.push((
                    row.status.is_running(),
                    task.start_time,
                    row.id.clone(),
                    row,
                ));
            }
            // local_agent rows carry their full state in the registry; every
            // other `Other` task type has no panel projection yet.
            TaskState::Other(_) => {}
        }
    }
    for task in local_agents {
        if !task.is_backgrounded {
            // Foreground sync subagent: shown in the message stream instead.
            continue;
        }
        if crate::tasks::local_main_session_task::is_main_session_task(task) {
            // The leader's own session is not a background worker.
            continue;
        }
        let row = local_agent_row(task, now_ms);
        sortable.push((
            row.status.is_running(),
            task.start_time_ms,
            row.id.clone(),
            row,
        ));
    }
    sortable.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    sortable.into_iter().map(|(_, _, _, row)| row).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackedStatus {
    Running,
    Terminal { since_ms: u64, hidden: bool },
}

/// Component-local record of when each id was first seen terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionTracker {
    entries: std::collections::HashMap<String, TrackedStatus>,
}

impl RetentionTracker {
    fn get(&self, id: &str) -> Option<TrackedStatus> {
        self.entries.get(id).copied()
    }
}

/// What one retention pass produced: the rows still visible, plus the new
/// `/tasks to see agents` deadline when a running row completed this frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub visible: Vec<AgentRow>,
    pub finished_hint_deadline_ms: Option<u64>,
}

/// Applies the §6.1 lifecycle to the projected rows.
///
/// - Running rows are visible and tracked.
/// - A row that completes disappears on the transition frame and starts the
///   `/tasks to see agents` hint window.
/// - Failed/killed rows stay visible for `failed_retention_ms` after the
///   transition.
/// - A row first seen already terminal is hidden forever.
///
/// Rows absent from `rows` are forgotten, so a recycled id starts fresh.
pub fn apply_terminal_retention(
    rows: &[AgentRow],
    tracker: &mut RetentionTracker,
    now_ms: u64,
    failed_retention_ms: u64,
    finished_hint_ms: u64,
) -> RetentionOutcome {
    let mut visible = Vec::new();
    let mut next = std::collections::HashMap::new();
    let mut finished_hint_deadline_ms: Option<u64> = None;
    for row in rows {
        let previous = tracker.get(&row.id);
        // A completed row vanishes the moment it is not running any more;
        // failed/killed rows keep the usual terminal grace window.
        let (tracked, keep) = match row.status {
            AgentStatus::Running => (TrackedStatus::Running, true),
            AgentStatus::Completed => {
                let tracked = match previous {
                    Some(TrackedStatus::Running) => {
                        finished_hint_deadline_ms = Some(now_ms.saturating_add(finished_hint_ms));
                        TrackedStatus::Terminal {
                            since_ms: now_ms,
                            hidden: true,
                        }
                    }
                    Some(tracked @ TrackedStatus::Terminal { .. }) => tracked,
                    None => TrackedStatus::Terminal {
                        since_ms: now_ms,
                        hidden: true,
                    },
                };
                (tracked, false)
            }
            AgentStatus::Failed | AgentStatus::Killed => {
                let tracked = match previous {
                    Some(TrackedStatus::Running) => TrackedStatus::Terminal {
                        since_ms: now_ms,
                        hidden: false,
                    },
                    Some(tracked @ TrackedStatus::Terminal { .. }) => tracked,
                    None => TrackedStatus::Terminal {
                        since_ms: now_ms,
                        hidden: true,
                    },
                };
                let keep = match tracked {
                    TrackedStatus::Terminal { since_ms, hidden } => {
                        !hidden && now_ms.saturating_sub(since_ms) < failed_retention_ms
                    }
                    TrackedStatus::Running => true,
                };
                (tracked, keep)
            }
        };
        next.insert(row.id.clone(), tracked);
        if keep {
            visible.push(row.clone());
        }
    }
    tracker.entries = next;
    RetentionOutcome {
        visible,
        finished_hint_deadline_ms,
    }
}

/// Row budget from terminal rows (§7). `None` hides the panel; `rows == 0`
/// is an unknown size and is treated as tall.
pub fn panel_rows_budget(terminal_rows: usize) -> Option<usize> {
    let rows = if terminal_rows == 0 {
        usize::MAX
    } else {
        terminal_rows
    };
    if rows >= TALL_ROWS {
        Some(MAX_ROWS_TALL)
    } else if rows >= MIN_ROWS_FOR_PANEL {
        Some(MAX_ROWS_MEDIUM)
    } else {
        None
    }
}

/// One panel render's line budget (§7). The panel and PromptInput's footer
/// height both use the published plan, so the reserved rows can never
/// disagree with the drawn ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PanelPlan {
    /// Task-list lines drawn, the `… +N more` line included.
    pub task_lines: usize,
    /// Rows folded into the overflow line.
    pub hidden_rows: usize,
    /// Tool-call lines expanded under the focused row.
    pub expanded_lines: usize,
    /// The focused key-hint line.
    pub hint_line: bool,
}

impl PanelPlan {
    pub fn total_lines(&self) -> usize {
        self.task_lines + self.expanded_lines + usize::from(self.hint_line)
    }
}

/// §7 height plan: `None` hides the panel (terminal too short or no rows).
pub fn plan_panel(
    terminal_rows: usize,
    row_count: usize,
    selected: Option<usize>,
    selected_activity_count: usize,
) -> Option<PanelPlan> {
    let tall = terminal_rows == 0 || terminal_rows >= TALL_ROWS;
    let max_height = if tall {
        MAX_PANEL_HEIGHT
    } else {
        MEDIUM_PANEL_HEIGHT
    };
    plan_panel_with_height_cap(
        terminal_rows,
        row_count,
        selected,
        selected_activity_count,
        max_height,
    )
}

/// The budget loop itself, with the height cap explicit so tests can drive
/// the sacrifice order. Order (§6.2): expanded tool calls first, then task
/// rows.
fn plan_panel_with_height_cap(
    terminal_rows: usize,
    row_count: usize,
    selected: Option<usize>,
    selected_activity_count: usize,
    max_height: usize,
) -> Option<PanelPlan> {
    let max_task_lines = panel_rows_budget(terminal_rows)?;
    if row_count == 0 {
        return None;
    }
    let tall = terminal_rows == 0 || terminal_rows >= TALL_ROWS;
    let task_lines = row_count.min(max_task_lines);
    let hidden_rows = row_count.saturating_sub(max_task_lines);
    // Only a drawn row can expand: with an overflow line the last task slot
    // belongs to `… +N more`.
    let drawn_rows = if hidden_rows > 0 {
        task_lines.saturating_sub(1)
    } else {
        task_lines
    };
    let selected_drawn = selected.filter(|index| *index < drawn_rows);
    // 16–23 rows: no expansion (§7).
    let expanded_lines = if tall {
        selected_drawn
            .map(|_| selected_activity_count.min(MAX_EXPANDED_ACTIVITIES))
            .unwrap_or(0)
    } else {
        0
    };
    Some(trim_plan_to_height(
        PanelPlan {
            task_lines,
            hidden_rows,
            expanded_lines,
            hint_line: selected.is_some(),
        },
        max_height,
    ))
}

/// Enforces a line cap on an already-planned panel: expanded calls go first,
/// then task rows, and the hint line last. A zero cap blanks the panel.
fn trim_plan_to_height(mut plan: PanelPlan, max_height: usize) -> PanelPlan {
    if max_height == 0 {
        return PanelPlan::default();
    }
    while plan.total_lines() > max_height {
        if plan.expanded_lines > 0 {
            plan.expanded_lines -= 1;
        } else if plan.task_lines > 1 {
            plan.task_lines -= 1;
            plan.hidden_rows += 1;
        } else if plan.hint_line {
            plan.hint_line = false;
        } else {
            break;
        }
    }
    plan
}

/// Splits the visible rows into the rows actually drawn and the number folded
/// into the trailing `… +N more` line. The overflow line consumes one of the
/// `task_lines` slots.
pub fn split_display_rows(rows: &[AgentRow], task_lines: usize) -> (&[AgentRow], usize) {
    if rows.len() <= task_lines {
        return (rows, 0);
    }
    let shown = task_lines.saturating_sub(1);
    (&rows[..shown], rows.len() - shown)
}

/// §6.1: the name column is the longest visible name, capped at 12 columns;
/// longer names are truncated into that column so the alignment holds.
fn fit_name(name: &str, name_width: usize) -> String {
    pad_to_width(&truncate_to_width(name, name_width), name_width)
}

/// Clamps a panel selection into `0..len`. `len == 0` returns 0; the footer
/// selection itself is cleared by PromptInput's vanished-pill logic.
pub fn clamped_agent_selection(selected: usize, len: usize) -> usize {
    selected.min(len.saturating_sub(1))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSpan {
    pub text: String,
    pub color: Option<Color>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPanelLine {
    pub prefix: String,
    pub prefix_color: Option<Color>,
    pub spans: Vec<AgentSpan>,
    pub right: Option<AgentSpan>,
    pub background: Option<Color>,
}

impl AgentPanelLine {
    pub fn text(&self) -> String {
        let mut text = self.prefix.clone();
        for span in &self.spans {
            text.push_str(&span.text);
        }
        if let Some(right) = &self.right {
            text.push_str("  ");
            text.push_str(&right.text);
        }
        text
    }
}

fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn pad_to_width(text: &str, target: usize) -> String {
    let padding = target.saturating_sub(width(text));
    format!("{text}{}", " ".repeat(padding))
}

fn name_column_width(rows: &[AgentRow]) -> usize {
    rows.iter()
        .map(|row| width(&row.name))
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_WIDTH)
}

fn icon_for(status: AgentStatus, tick: usize) -> (&'static str, ThemeColorKey) {
    let figs = figures::get();
    match status {
        AgentStatus::Running => (
            crate::components::spinner::utils::spinner_frame(tick),
            ThemeColorKey::Inactive,
        ),
        AgentStatus::Completed => (figs.tick, ThemeColorKey::Success),
        AgentStatus::Failed | AgentStatus::Killed => (figs.cross, ThemeColorKey::Error),
    }
}

/// Key-hint line for the focused panel (§6.2). Callers pass the display text
/// of the live `footer:*` bindings so a user remap is reflected here.
pub fn format_panel_hint(up: &str, down: &str, enter: &str, stop: &str, back: &str) -> String {
    let select = if up == "↑" && down == "↓" {
        "↑↓".to_string()
    } else {
        format!("{up}/{down}")
    };
    format!("{select} select · {enter} details · {stop} stop · {back} back")
}

/// The hint line with the default/merged keybindings, resolved lazily.
///
/// `get_shortcut_display` is last-wins, and the Footer context binds ↑/↓ twice
/// (`up`/`ctrl+p`, `down`/`ctrl+n`), so the raw lookup would print `ctrl+p`.
/// The arrow is still an actual binding for the action, so prefer it when it
/// survives the user's merge and let a rebind win otherwise.
fn panel_hint_shortcut(action: &str, arrow: &str, fallback: &str) -> String {
    use crate::keybindings::parser::chord_to_string;
    let arrow_bound = crate::keybindings::load_user_bindings::load_keybindings_sync()
        .iter()
        .any(|binding| {
            binding.action.as_deref() == Some(action)
                && binding.context == ContextName::Footer
                && chord_to_string(&binding.chord) == arrow
        });
    if arrow_bound {
        return arrow.to_string();
    }
    crate::keybindings::shortcut_format::get_shortcut_display(
        action,
        &ContextName::Footer,
        fallback,
    )
}

pub fn panel_hint_line() -> String {
    format_panel_hint(
        &panel_hint_shortcut("footer:up", "↑", "↑"),
        &panel_hint_shortcut("footer:down", "↓", "↓"),
        &panel_hint_shortcut("footer:openSelected", "enter", "Enter"),
        &panel_hint_shortcut("footer:stopSelected", "x", "x"),
        &panel_hint_shortcut("footer:clearSelection", "escape", "Esc"),
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentPanelData {
    /// Rows that survived the terminal lifecycle, sorted.
    pub rows: Vec<AgentRow>,
    /// Terminal rows; `None` hides the panel.
    pub terminal_rows: usize,
    /// Selection inside the displayed rows; only when the footer focuses the
    /// panel.
    pub selected: Option<usize>,
    /// 0 reads as 80.
    pub columns: usize,
    /// Spinner frame index (1 Hz, §6.1).
    pub tick: usize,
}

fn selected_activity_count(rows: &[AgentRow], selected: Option<usize>) -> usize {
    selected
        .and_then(|index| rows.get(index))
        .map(|row| row.recent_activities.len())
        .unwrap_or(0)
}

/// Lines for one panel render. Pure; the component only paints them.
pub fn layout_agent_panel(data: &AgentPanelData, theme: &Theme) -> Vec<AgentPanelLine> {
    let Some(plan) = plan_panel(
        data.terminal_rows,
        data.rows.len(),
        data.selected,
        selected_activity_count(&data.rows, data.selected),
    ) else {
        return Vec::new();
    };
    let columns = if data.columns == 0 { 80 } else { data.columns };
    let (shown, hidden) = split_display_rows(&data.rows, plan.task_lines);
    let name_width = name_column_width(shown);
    let inactive = Some(theme.inactive);
    let mut lines = Vec::with_capacity(plan.total_lines());
    for (index, row) in shown.iter().enumerate() {
        let is_selected = data.selected == Some(index);
        let prefix = if is_selected {
            SELECTED_PREFIX
        } else {
            UNSELECTED_PREFIX
        };
        let (icon, icon_key) = icon_for(row.status, data.tick);
        let icon_text = format!("{icon} ");
        let name = fit_name(&row.name, name_width);
        let fixed = width(prefix) + width(&icon_text) + name_width + ROW_GAP;
        let stats_width = if row.stats.is_empty() {
            0
        } else {
            width(&row.stats) + ROW_GAP
        };
        let activity_budget = columns.saturating_sub(fixed);
        let keep_stats = !row.stats.is_empty()
            && activity_budget >= stats_width + MIN_ACTIVITY_WIDTH
            && columns >= fixed + stats_width;
        let activity_budget = if keep_stats {
            activity_budget.saturating_sub(stats_width)
        } else {
            activity_budget
        };
        let activity = if row.activity.is_empty() || activity_budget == 0 {
            String::new()
        } else {
            truncate_to_width(&row.activity, activity_budget)
        };
        let separator = if activity.is_empty() { "" } else { "  " };
        let mut spans = vec![
            AgentSpan {
                text: icon_text,
                color: Some(theme.color(icon_key)),
            },
            AgentSpan {
                text: name,
                color: row
                    .name_color
                    .map(|key| theme.color(key))
                    .or(Some(theme.text)),
            },
        ];
        if !activity.is_empty() {
            spans.push(AgentSpan {
                text: format!("{separator}{activity}"),
                color: inactive,
            });
        }
        lines.push(AgentPanelLine {
            prefix: prefix.to_string(),
            prefix_color: if is_selected {
                Some(theme.nyan)
            } else {
                inactive
            },
            spans,
            right: keep_stats.then(|| AgentSpan {
                text: row.stats.clone(),
                color: inactive,
            }),
            background: is_selected.then_some(theme.selection_bg),
        });
        if is_selected && plan.expanded_lines > 0 {
            // Newest calls last; trimming drops the oldest (§6.2).
            let activities =
                &row.recent_activities[row.recent_activities.len() - plan.expanded_lines..];
            for (activity_index, activity) in activities.iter().enumerate() {
                let prefix = if activity_index == 0 {
                    EXPANDED_PREFIX
                } else {
                    EXPANDED_CONTINUATION
                };
                let budget = columns.saturating_sub(width(prefix));
                lines.push(AgentPanelLine {
                    prefix: prefix.to_string(),
                    prefix_color: inactive,
                    spans: vec![AgentSpan {
                        text: truncate_to_width(activity, budget.max(1)),
                        color: inactive,
                    }],
                    right: None,
                    background: None,
                });
            }
        }
    }
    if hidden > 0 {
        let budget = columns.saturating_sub(width(UNSELECTED_PREFIX));
        lines.push(AgentPanelLine {
            prefix: UNSELECTED_PREFIX.to_string(),
            prefix_color: inactive,
            spans: vec![AgentSpan {
                text: truncate_to_width(&format!("… +{hidden} more"), budget.max(1)),
                color: inactive,
            }],
            right: None,
            background: None,
        });
    }
    if plan.hint_line {
        let budget = columns.saturating_sub(width(HINT_PREFIX));
        lines.push(AgentPanelLine {
            prefix: HINT_PREFIX.to_string(),
            prefix_color: inactive,
            spans: vec![AgentSpan {
                text: truncate_to_width(&panel_hint_line(), budget.max(1)),
                color: inactive,
            }],
            right: None,
            background: None,
        });
    }
    lines
}

fn span_element(span: &AgentSpan) -> AnyElement<'static> {
    element! {
        Text(content: span.text.clone(), color: span.color, wrap: TextWrap::NoWrap)
    }
    .into_any()
}

fn line_element(line: &AgentPanelLine) -> AnyElement<'static> {
    let mut cells: Vec<AnyElement<'static>> = vec![
        element! {
            Text(content: line.prefix.clone(), color: line.prefix_color, wrap: TextWrap::NoWrap)
        }
        .into_any(),
    ];
    cells.extend(line.spans.iter().map(span_element));
    if let Some(right) = &line.right {
        cells.push(element! { View(flex_grow: 1.0f32) }.into_any());
        cells.push(span_element(right));
    }
    element! {
        View(
            flex_direction: FlexDirection::Row,
            width: 100pct,
            height: 1u32,
            overflow: Overflow::Hidden,
            background_color: line.background,
        ) {
            #(cells)
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub struct AgentPanelViewProps {
    pub data: AgentPanelData,
}

/// Presentational half: paints [`layout_agent_panel`] and nothing else.
#[component]
pub fn AgentPanelView(props: &AgentPanelViewProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let lines = layout_agent_panel(&props.data, &theme);
    if lines.is_empty() {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            #(lines.iter().map(line_element).collect::<Vec<_>>())
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub struct AgentPanelProps {
    /// Override the terminal size (tests); `None` reads the terminal.
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    /// Frozen "now" for deterministic tests; `None` reads the wall clock.
    pub now_ms: Option<u64>,
    /// Frozen spinner frame for static renders.
    pub tick: Option<usize>,
}

fn items_signature(items: &[AgentPanelItem]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in items {
        item.id.hash(&mut hasher);
        (item.kind as u8).hash(&mut hasher);
    }
    items.len().hash(&mut hasher);
    hasher.finish()
}

/// One tracker for the whole process.
///
/// @nyan: the panel mounts once (inside `Footer`), but PromptInput — and with
/// it the whole footer subtree — is keyed as `prompt-{generation}` and
/// remounts on every submit/cancel (`screens/repl.rs`). A per-instance
/// `use_ref` would lose "was running" across that remount, so a task that
/// finished right at the turn boundary would be first-seen-terminal by the new
/// instance and skip its ✘/hint. The tracker therefore stays process-wide.
fn shared_retention_tracker() -> &'static std::sync::Mutex<RetentionTracker> {
    static TRACKER: std::sync::OnceLock<std::sync::Mutex<RetentionTracker>> =
        std::sync::OnceLock::new();
    TRACKER.get_or_init(|| std::sync::Mutex::new(RetentionTracker::default()))
}

#[component]
pub fn AgentPanel(props: &AgentPanelProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (terminal_columns, terminal_rows) = hooks.use_terminal_size();
    let tasks = use_app_state(&mut hooks, |state| state.tasks.clone());
    let selected_index = use_app_state(&mut hooks, |state| {
        (state.footer_selection == Some(FooterItem::AgentPanel))
            .then_some(state.agent_panel_selected)
    });
    let store = use_set_app_state(&mut hooks);
    let finished_hint_until_ms =
        use_app_state(&mut hooks, |state| state.agent_panel_finished_hint_until_ms);
    let columns = props.columns.unwrap_or(terminal_columns as u32) as usize;
    let terminal_rows = props.rows.unwrap_or(terminal_rows as u32) as usize;
    let now_ms = props
        .now_ms
        .unwrap_or_else(crate::utils::task::framework::now_ms);
    let budget = panel_rows_budget(terminal_rows);
    let mut visible = Vec::new();
    let mut finished_hint_deadline_ms = None;
    if budget.is_some() {
        let local_agents = crate::tasks::local_agent_task::local_agent_tasks_snapshot();
        let projected = project_agent_rows(&tasks, &local_agents, now_ms);
        let outcome = apply_terminal_retention(
            &projected,
            &mut shared_retention_tracker()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            now_ms,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        visible = outcome.visible;
        finished_hint_deadline_ms = outcome.finished_hint_deadline_ms;
    }
    let displayed = plan_panel(
        terminal_rows,
        visible.len(),
        selected_index,
        selected_activity_count(&visible, selected_index),
    )
    .map(|plan| split_display_rows(&visible, plan.task_lines).0.to_vec())
    .unwrap_or_default();
    // @nyan: the 1 s tick drives the spinner frame, elapsed times, the
    // failed-row expiry and the finished-hint expiry; it keeps running while
    // the `/tasks to see agents` hint is still fresh, even with no rows left
    // (§6.1), and stops otherwise.
    let hint_ticking = finished_hint_until_ms.is_some_and(|until| now_ms < until);
    let interval = (!visible.is_empty() || hint_ticking)
        .then(|| std::time::Duration::from_millis(TICK_INTERVAL_MS));
    let frame = hooks.use_animation_frame(interval);
    let tick = props
        .tick
        .unwrap_or((frame.time_ms as u64 / TICK_INTERVAL_MS) as usize);

    let items: Vec<AgentPanelItem> = displayed
        .iter()
        .map(|row| AgentPanelItem {
            id: row.id.clone(),
            kind: row.kind,
            activity_count: row.recent_activities.len().min(MAX_EXPANDED_ACTIVITIES),
        })
        .collect();
    let signature = items_signature(&items);
    let len = items.len();
    let items_for_effect = items.clone();
    let row_count = visible.len();
    let now_for_effect = now_ms;
    hooks.use_effect(
        move || {
            store.set_state(|prev| {
                let mut next = (**prev).clone();
                let mut changed = false;
                if next.agent_panel_items.as_slice() != items_for_effect.as_slice() {
                    next.agent_panel_items = Arc::new(items_for_effect.clone());
                    changed = true;
                }
                if next.agent_panel_row_count != row_count {
                    next.agent_panel_row_count = row_count;
                    changed = true;
                }
                // `Some` only on the transition frame (§6.1); later frames
                // hold the tracked terminal status and do not re-arm.
                if let Some(deadline) = finished_hint_deadline_ms
                    && next.agent_panel_finished_hint_until_ms != Some(deadline)
                {
                    next.agent_panel_finished_hint_until_ms = Some(deadline);
                    changed = true;
                }
                // Expire the hint so the mode row stops showing it even when
                // no further task transition arrives.
                if let Some(until) = next.agent_panel_finished_hint_until_ms
                    && now_for_effect >= until
                {
                    next.agent_panel_finished_hint_until_ms = None;
                    changed = true;
                }
                let clamped = clamped_agent_selection(next.agent_panel_selected, len);
                if next.agent_panel_selected != clamped {
                    next.agent_panel_selected = clamped;
                    changed = true;
                }
                if changed {
                    crate::state::store::UpdateDecision::Replace {
                        next: Arc::new(next),
                        result: (),
                    }
                } else {
                    crate::state::store::UpdateDecision::Same(())
                }
            });
        },
        (signature, len, row_count, now_ms / TICK_INTERVAL_MS),
    );

    let data = AgentPanelData {
        // The full sorted list: `layout_agent_panel` re-runs the same
        // `plan_panel` the publish above used, so the drawn rows and
        // `agent_panel_items` cannot disagree.
        rows: visible,
        terminal_rows,
        selected: selected_index,
        columns,
        tick,
    };
    element! { AgentPanelView(data: data) }.into_any()
}

/// Shortcut text for the footer/hint copy; shared by tests.
pub(crate) fn agent_panel_item_at(
    items: &[AgentPanelItem],
    selected: usize,
) -> Option<&AgentPanelItem> {
    items.get(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn snapshot(
        id: &str,
        name: &str,
        status: &str,
    ) -> crate::components::spinner::teammate_tree::TeammateTaskSnapshot {
        crate::components::spinner::teammate_tree::TeammateTaskSnapshot {
            id: id.to_string(),
            task_type: crate::components::spinner::teammate_tree::IN_PROCESS_TEAMMATE_TASK_TYPE
                .to_string(),
            status: status.to_string(),
            agent_name: name.to_string(),
            color: Some("purple".to_string()),
            tool_use_count: 3,
            token_count: 12_000,
            last_activity_description: Some("Reviewing diff".to_string()),
            ..Default::default()
        }
    }

    fn shell(id: &str, command: &str, status: &str, start_ms: i64) -> LocalShellTaskState {
        LocalShellTaskState {
            id: id.to_string(),
            task_type: "local_bash".to_string(),
            status: status.to_string(),
            description: format!("{command} description"),
            command: command.to_string(),
            result: None,
            notified: false,
            shell_command: None,
            last_reported_total_lines: 0,
            is_backgrounded: true,
            agent_id: None,
            tool_use_id: None,
            kind: None,
            start_time_ms: start_ms,
            end_time_ms: None,
        }
    }

    fn agent(id: &str, agent_type: &str, status: &str, start_ms: u64) -> LocalAgentTaskState {
        LocalAgentTaskState {
            task_id: id.to_string(),
            task_type: "local_agent".to_string(),
            status: status.to_string(),
            agent_id: id.to_string(),
            prompt: "prompt".to_string(),
            selected_agent: None,
            agent_type: agent_type.to_string(),
            description: "survey the TUI".to_string(),
            model: None,
            error: None,
            result: None,
            progress: Some(crate::tasks::local_agent_task::AgentProgress {
                tool_use_count: 14,
                token_count: 38_000,
                last_activity: None,
                recent_activities: vec![crate::tasks::local_agent_task::ToolActivity {
                    tool_name: "Read".to_string(),
                    input: serde_json::Value::Null,
                    activity_description: Some("调研 TUI 结构".to_string()),
                    is_search: false,
                    is_read: true,
                }],
                summary: None,
            }),
            retrieved: false,
            messages: Vec::new(),
            last_reported_tool_count: 0,
            last_reported_token_count: 0,
            is_backgrounded: true,
            pending_messages: Vec::new(),
            retain: false,
            disk_loaded: false,
            start_time_ms: start_ms,
            end_time_ms: None,
            tool_use_id: None,
            output_file: String::new(),
            abort_controller: Default::default(),
            notified: false,
            evict_after: None,
        }
    }

    fn dream(id: &str, status: &str) -> DreamTaskState {
        DreamTaskState {
            id: id.to_string(),
            task_type: "dream".to_string(),
            status: status.to_string(),
            description: "dreaming".to_string(),
            notified: false,
            start_time: 90_000,
            end_time: None,
            phase: crate::tasks::dream_task::DreamPhase::Updating,
            sessions_reviewing: 1,
            files_touched: Vec::new(),
            turns: Vec::new(),
            abort_controller: None,
            prior_mtime: 0,
            lock: None,
        }
    }

    fn tasks_map(entries: Vec<TaskState>) -> BTreeMap<String, Arc<TaskState>> {
        entries
            .into_iter()
            .map(|task| {
                let id = task.id().to_string();
                (id, Arc::new(task))
            })
            .collect()
    }

    #[test]
    fn projection_covers_all_four_sources_and_stats_formats() {
        // now = 130_000; agent started 130s ago (2m10s), shell 45s ago.
        let tasks = tasks_map(vec![
            TaskState::InProcessTeammate(snapshot("team-1", "alpha", "running")),
            TaskState::LocalShell(shell("sh-1", "cargo test", "running", 85_000)),
            TaskState::Dream(dream("dream-1", "running")),
        ]);
        let rows = project_agent_rows(&tasks, &[agent("a-1", "explore", "running", 0)], 130_000);
        assert_eq!(rows.len(), 4);

        let by_id = |id: &str| rows.iter().find(|row| row.id == id).unwrap();
        let agent_row = by_id("a-1");
        assert_eq!(agent_row.kind, AgentKind::Agent);
        assert_eq!(agent_row.name, "explore");
        assert_eq!(agent_row.activity, "调研 TUI 结构");
        assert_eq!(agent_row.stats, "14 tools · 38k · 2m10s");
        assert_eq!(agent_row.name_color, None, "unmapped agent type");

        let teammate = by_id("team-1");
        assert_eq!(teammate.kind, AgentKind::Teammate);
        assert_eq!(teammate.name, "@alpha");
        assert_eq!(teammate.activity, "Reviewing diff");
        assert_eq!(teammate.stats, "3 tools · 12k");
        assert_eq!(teammate.name_color, Some(ThemeColorKey::AgentPurple));

        let shell_row = by_id("sh-1");
        assert_eq!(shell_row.kind, AgentKind::Shell);
        assert_eq!(shell_row.name, "$");
        assert_eq!(shell_row.activity, "cargo test");
        assert_eq!(shell_row.stats, "45s");

        let dream_row = by_id("dream-1");
        assert_eq!(dream_row.kind, AgentKind::Dream);
        assert_eq!(dream_row.name, "dream");
        assert_eq!(dream_row.activity, "updating · dreaming");
        assert_eq!(dream_row.stats, "40s");
    }

    #[test]
    fn agent_color_comes_from_the_shared_agent_color_manager() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        crate::tools::agent_tool::agent_color_manager::set_agent_color(
            "explore-colored",
            Some(crate::tools::agent_tool::agent_color_manager::AgentColorName::Blue),
        );
        let rows = project_agent_rows(
            &BTreeMap::new(),
            &[agent("a-1", "explore-colored", "running", 0)],
            0,
        );
        assert_eq!(rows[0].name_color, Some(ThemeColorKey::AgentBlue));
        crate::tools::agent_tool::agent_color_manager::set_agent_color("explore-colored", None);
    }

    #[test]
    fn terminal_status_mapping_includes_succeeded_and_foreground_is_excluded() {
        let tasks = tasks_map(vec![
            TaskState::LocalShell(shell("done", "make", "completed", 1)),
            TaskState::LocalShell(shell("ok", "make", "succeeded", 1)),
            TaskState::LocalShell(shell("bad", "make", "failed", 1)),
            TaskState::LocalShell(shell("stop", "make", "killed", 1)),
        ]);
        let mut foreground = agent("fg", "explore", "running", 1);
        foreground.is_backgrounded = false;
        let rows = project_agent_rows(&tasks, &[foreground], 10);
        let status = |id: &str| rows.iter().find(|row| row.id == id).unwrap().status;
        assert_eq!(status("done"), AgentStatus::Completed);
        assert_eq!(status("ok"), AgentStatus::Completed);
        assert_eq!(status("bad"), AgentStatus::Failed);
        assert_eq!(status("stop"), AgentStatus::Killed);
        assert!(rows.iter().all(|row| row.id != "fg"));
    }

    #[test]
    fn sort_is_running_first_then_start_desc_then_id() {
        let tasks = tasks_map(vec![
            TaskState::LocalShell(shell("z-running", "z", "running", 5)),
            TaskState::LocalShell(shell("a-running", "a", "running", 5)),
            TaskState::LocalShell(shell("newer-terminal", "n", "completed", 9)),
            TaskState::LocalShell(shell("older-running", "o", "running", 1)),
            TaskState::InProcessTeammate(snapshot("team", "t", "running")),
        ]);
        let rows = project_agent_rows(&tasks, &[], 10);
        let ids = rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "a-running",
                "z-running",
                "older-running",
                // Teammates have no start time.
                "team",
                "newer-terminal",
            ]
        );
    }

    #[test]
    fn max_rows_folds_the_tail_into_more() {
        let rows = (0..6)
            .map(|index| AgentRow {
                id: format!("r{index}"),
                kind: AgentKind::Shell,
                name: "$".to_string(),
                name_color: None,
                activity: "cmd".to_string(),
                stats: "1s".to_string(),
                status: AgentStatus::Running,
                recent_activities: Vec::new(),
            })
            .collect::<Vec<_>>();
        let data = AgentPanelData {
            rows: rows.clone(),
            terminal_rows: 30,
            selected: None,
            columns: 80,
            tick: 0,
        };
        let lines = layout_agent_panel(&data, &*theme::current());
        assert_eq!(lines.len(), 4);
        assert!(
            lines[3].text().contains("… +3 more"),
            "{:?}",
            lines[3].text()
        );
        assert!(lines[3].prefix.starts_with("  "));

        let data = AgentPanelData {
            rows: rows[..4].to_vec(),
            terminal_rows: 30,
            ..data
        };
        let lines = layout_agent_panel(&data, &*theme::current());
        assert_eq!(lines.len(), 4);
        assert!(!lines.iter().any(|line| line.text().contains("more")));
    }

    #[test]
    fn names_align_in_a_column_capped_at_twelve() {
        let rows = vec![
            AgentRow {
                id: "1".into(),
                kind: AgentKind::Agent,
                name: "explore".into(),
                name_color: None,
                activity: "one".into(),
                stats: String::new(),
                status: AgentStatus::Running,
                recent_activities: Vec::new(),
            },
            AgentRow {
                id: "2".into(),
                kind: AgentKind::Teammate,
                name: "@a-much-longer-name".into(),
                name_color: None,
                activity: "two".into(),
                stats: String::new(),
                status: AgentStatus::Running,
                recent_activities: Vec::new(),
            },
        ];
        let data = AgentPanelData {
            rows,
            terminal_rows: 30,
            selected: None,
            columns: 80,
            tick: 0,
        };
        let lines = layout_agent_panel(&data, &*theme::current());
        // Display columns, not byte offsets: the marker and `…` are
        // multi-byte.
        let activity_column = |text: &str, activity: &str| {
            let byte = text.find(activity).unwrap();
            width(&text[..byte])
        };
        assert_eq!(activity_column(&lines[0].text(), "one"), 18);
        assert_eq!(activity_column(&lines[1].text(), "two"), 18);
        // 2 prefix + 2 icon + 12 name + 2 gap
        assert_eq!(
            width(&lines[1].text()[..lines[1].text().find("two").unwrap()]),
            18
        );
        assert!(lines[1].text().contains("@a-much-lon…"));
    }

    #[test]
    fn narrow_widths_drop_stats_then_truncate_activity() {
        let rows = vec![AgentRow {
            id: "1".into(),
            kind: AgentKind::Agent,
            name: "explore".into(),
            name_color: None,
            activity: "a very long activity description that must be cut".into(),
            stats: "14 tools · 38k · 2m10s".into(),
            status: AgentStatus::Running,
            recent_activities: Vec::new(),
        }];
        let theme = *theme::current();
        for (columns, expect_stats) in [(80usize, true), (60, true), (40, false)] {
            let data = AgentPanelData {
                rows: rows.clone(),
                terminal_rows: 30,
                selected: None,
                columns,
                tick: 0,
            };
            let lines = layout_agent_panel(&data, &theme);
            let line = &lines[0];
            assert_eq!(
                line.right.is_some(),
                expect_stats,
                "columns={columns}: {}",
                line.text()
            );
            assert!(
                width(&line.text()) <= columns,
                "columns={columns}: {}",
                line.text()
            );
            if !expect_stats {
                assert!(
                    line.text().ends_with('…'),
                    "columns={columns}: {}",
                    line.text()
                );
            }
        }
    }

    #[test]
    fn completed_rows_hide_at_once_and_arm_the_hint() {
        let row = |status| AgentRow {
            id: "r1".to_string(),
            kind: AgentKind::Shell,
            name: "$".to_string(),
            name_color: None,
            activity: "cmd".to_string(),
            stats: "1s".to_string(),
            status,
            recent_activities: Vec::new(),
        };

        // First seen terminal -> hidden, and no hint (it is not a transition).
        let mut tracker = RetentionTracker::default();
        let outcome = apply_terminal_retention(
            &[row(AgentStatus::Completed)],
            &mut tracker,
            1_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert!(outcome.visible.is_empty());
        assert_eq!(outcome.finished_hint_deadline_ms, None);

        // running -> completed: gone on the transition frame, hint armed.
        let mut tracker = RetentionTracker::default();
        let outcome = apply_terminal_retention(
            &[row(AgentStatus::Running)],
            &mut tracker,
            1_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert_eq!(outcome.visible.len(), 1);
        let outcome = apply_terminal_retention(
            &[row(AgentStatus::Completed)],
            &mut tracker,
            2_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert!(outcome.visible.is_empty());
        assert_eq!(
            outcome.finished_hint_deadline_ms,
            Some(2_000 + FINISHED_HINT_MS)
        );
        // Later frames do not re-arm.
        let outcome = apply_terminal_retention(
            &[row(AgentStatus::Completed)],
            &mut tracker,
            3_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert!(outcome.visible.is_empty());
        assert_eq!(outcome.finished_hint_deadline_ms, None);
    }

    #[test]
    fn failed_rows_linger_thirty_seconds_and_first_seen_terminal_stays_hidden() {
        let row = |status| AgentRow {
            id: "r1".to_string(),
            kind: AgentKind::Shell,
            name: "$".to_string(),
            name_color: None,
            activity: "cmd".to_string(),
            stats: "1s".to_string(),
            status,
            recent_activities: Vec::new(),
        };
        let mut tracker = RetentionTracker::default();
        assert!(
            apply_terminal_retention(
                &[row(AgentStatus::Failed)],
                &mut tracker,
                1_000,
                FAILED_RETENTION_MS,
                FINISHED_HINT_MS,
            )
            .visible
            .is_empty(),
            "first seen terminal -> hidden"
        );

        let mut tracker = RetentionTracker::default();
        let running = apply_terminal_retention(
            &[row(AgentStatus::Running)],
            &mut tracker,
            1_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert_eq!(running.visible.len(), 1);
        let failed = apply_terminal_retention(
            &[row(AgentStatus::Failed)],
            &mut tracker,
            2_000,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert_eq!(failed.visible.len(), 1, "the transition frame keeps the ✘");
        assert_eq!(
            failed.finished_hint_deadline_ms, None,
            "failed arms no hint"
        );
        // 30_000 ms minus one is inside the window; exactly 30 s is out.
        assert_eq!(
            apply_terminal_retention(
                &[row(AgentStatus::Killed)],
                &mut tracker,
                2_000 + FAILED_RETENTION_MS - 1,
                FAILED_RETENTION_MS,
                FINISHED_HINT_MS,
            )
            .visible
            .len(),
            1
        );
        assert!(
            apply_terminal_retention(
                &[row(AgentStatus::Killed)],
                &mut tracker,
                2_000 + FAILED_RETENTION_MS,
                FAILED_RETENTION_MS,
                FINISHED_HINT_MS,
            )
            .visible
            .is_empty()
        );
    }

    #[test]
    fn retention_forgets_ids_that_leave_the_projection() {
        let row = AgentRow {
            id: "r1".to_string(),
            kind: AgentKind::Shell,
            name: "$".to_string(),
            name_color: None,
            activity: "cmd".to_string(),
            stats: String::new(),
            status: AgentStatus::Running,
            recent_activities: Vec::new(),
        };
        let mut tracker = RetentionTracker::default();
        apply_terminal_retention(
            std::slice::from_ref(&row),
            &mut tracker,
            1,
            FAILED_RETENTION_MS,
            FINISHED_HINT_MS,
        );
        assert_eq!(tracker.get("r1"), Some(TrackedStatus::Running));
        apply_terminal_retention(&[], &mut tracker, 2, FAILED_RETENTION_MS, FINISHED_HINT_MS);
        assert_eq!(tracker.get("r1"), None);
    }

    #[test]
    fn row_budget_tiers_for_30_20_and_12_rows() {
        assert_eq!(panel_rows_budget(30), Some(4));
        assert_eq!(panel_rows_budget(24), Some(4));
        assert_eq!(panel_rows_budget(23), Some(2));
        assert_eq!(panel_rows_budget(20), Some(2));
        assert_eq!(panel_rows_budget(16), Some(2));
        assert_eq!(panel_rows_budget(15), None);
        assert_eq!(panel_rows_budget(12), None);
        assert_eq!(panel_rows_budget(0), Some(4), "unknown size reads as tall");
    }

    #[test]
    fn selection_is_clamped_into_the_displayed_range() {
        assert_eq!(clamped_agent_selection(5, 3), 2);
        assert_eq!(clamped_agent_selection(1, 3), 1);
        assert_eq!(clamped_agent_selection(2, 0), 0);
    }

    #[test]
    fn activity_uses_the_latest_tool_call_and_falls_back_to_the_description() {
        // Local agent: last recorded call, with the tool name when the
        // description is missing.
        let mut task = agent("a-1", "explore", "running", 0);
        let progress = task.progress.as_mut().unwrap();
        progress.recent_activities = vec![
            crate::tasks::local_agent_task::ToolActivity {
                tool_name: "Grep".to_string(),
                input: serde_json::Value::Null,
                activity_description: Some("Grep \"AgentPanel\" in src/".to_string()),
                is_search: true,
                is_read: false,
            },
            crate::tasks::local_agent_task::ToolActivity {
                tool_name: "Read".to_string(),
                input: serde_json::Value::Null,
                activity_description: None,
                is_search: false,
                is_read: true,
            },
        ];
        let row = local_agent_row(&task, 0);
        assert_eq!(row.activity, "Read", "latest call, tool-name fallback");
        assert_eq!(
            row.recent_activities,
            vec!["Grep \"AgentPanel\" in src/", "Read"],
            "expansion keeps the same fallback"
        );

        // No progress at all: the task description (§6.1 fallback).
        task.progress = None;
        assert_eq!(local_agent_row(&task, 0).activity, "survey the TUI");

        // Teammate: last `recent_activities` entry.
        let mut snapshot = snapshot("team-1", "alpha", "running");
        snapshot.last_activity_description = None;
        snapshot.recent_activities = vec![
            crate::components::spinner::teammate_tree::TeammateRecentActivity {
                activity_description: Some("Reading src/lib.rs".to_string()),
                is_search: false,
                is_read: true,
            },
            crate::components::spinner::teammate_tree::TeammateRecentActivity {
                activity_description: Some("Running tests".to_string()),
                is_search: false,
                is_read: false,
            },
        ];
        let row = teammate_row(&snapshot);
        assert_eq!(row.activity, "Running tests");
        assert_eq!(
            row.recent_activities,
            vec!["Reading src/lib.rs", "Running tests"]
        );

        // Shell and dream never expand (§6.2).
        assert!(
            shell_row(&shell("s", "cargo test", "running", 0), 1)
                .recent_activities
                .is_empty()
        );
        assert!(
            dream_row(&dream("d", "running"), 1)
                .recent_activities
                .is_empty()
        );
    }

    #[test]
    fn expanded_activities_cap_at_three_from_the_tail() {
        let mut task = agent("a-1", "explore", "running", 0);
        task.progress.as_mut().unwrap().recent_activities = (0..5)
            .map(|index| crate::tasks::local_agent_task::ToolActivity {
                tool_name: format!("Tool{index}"),
                input: serde_json::Value::Null,
                activity_description: None,
                is_search: false,
                is_read: false,
            })
            .collect();
        assert_eq!(
            local_agent_activities(&task),
            vec!["Tool2", "Tool3", "Tool4"],
            "newest three, oldest first"
        );
    }

    #[test]
    fn panel_hint_copy_matches_the_default_bindings() {
        assert_eq!(
            format_panel_hint("↑", "↓", "Enter", "x", "Esc"),
            "↑↓ select · Enter details · x stop · Esc back"
        );
        assert_eq!(
            format_panel_hint("ctrl+p", "ctrl+n", "Enter", "x", "Esc"),
            "ctrl+p/ctrl+n select · Enter details · x stop · Esc back",
            "a remapped pair prints both keys"
        );
        // The live resolver prefers the arrow bindings over the ctrl aliases.
        assert!(
            panel_hint_line().starts_with("↑↓ select · "),
            "{}",
            panel_hint_line()
        );
    }

    fn mixed_rows() -> Vec<AgentRow> {
        vec![
            AgentRow {
                id: "a".into(),
                kind: AgentKind::Agent,
                name: "explore".into(),
                name_color: Some(ThemeColorKey::AgentBlue),
                activity: "调研 TUI 结构".into(),
                stats: "14 tools · 38k · 2m10s".into(),
                status: AgentStatus::Running,
                recent_activities: vec![
                    "Grep \"AgentPanel\" in src/".into(),
                    "Read src/screens/repl.rs".into(),
                    "Read src/components/agent_panel.rs".into(),
                ],
            },
            AgentRow {
                id: "t".into(),
                kind: AgentKind::Teammate,
                name: "@alpha".into(),
                name_color: Some(ThemeColorKey::AgentPurple),
                activity: "Reviewing diff".into(),
                stats: "3 tools · 12k".into(),
                status: AgentStatus::Running,
                recent_activities: vec!["Reviewing diff".into()],
            },
            AgentRow {
                id: "s".into(),
                kind: AgentKind::Shell,
                name: "$".into(),
                name_color: None,
                activity: "cargo test".into(),
                stats: "45s".into(),
                status: AgentStatus::Completed,
                recent_activities: Vec::new(),
            },
            AgentRow {
                id: "f".into(),
                kind: AgentKind::Dream,
                name: "dream".into(),
                name_color: None,
                activity: "updating · dreaming".into(),
                stats: String::new(),
                status: AgentStatus::Failed,
                recent_activities: Vec::new(),
            },
        ]
    }

    fn render(data: AgentPanelData, width: usize) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                AgentPanelView(data: data)
            }
        }
        .render(Some(width))
    }

    struct CellStyle {
        color: Option<Color>,
        background: Option<Color>,
    }

    fn style_at(canvas: &Canvas, needle: &str) -> CellStyle {
        let text = canvas.to_string();
        let line_index = text
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in canvas=\n{text}"));
        let line = text.lines().nth(line_index).unwrap();
        let x = width(&line[..line.find(needle).unwrap()]);
        let style = canvas
            .resolved_text_style(x, line_index)
            .unwrap_or_else(|| panic!("no style at {needle:?} in canvas=\n{text}"));
        let background = canvas
            .cell(x, line_index)
            .and_then(|cell| cell.background_color);
        CellStyle {
            color: style.color,
            background,
        }
    }

    #[test]
    fn canvas_draws_status_markers_and_name_colors() {
        let data = AgentPanelData {
            rows: mixed_rows(),
            terminal_rows: 30,
            selected: None,
            columns: 80,
            tick: 0,
        };
        let canvas = render(data, 80);
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        // Every unselected row starts with a two-space marker column.
        assert!(lines[0].starts_with("  "), "canvas=\n{text}");
        assert!(lines[1].starts_with("  "), "canvas=\n{text}");
        assert!(lines[2].starts_with("  "), "canvas=\n{text}");
        assert!(lines[3].starts_with("  "), "canvas=\n{text}");
        assert!(!text.contains('├'), "canvas=\n{text}");
        assert!(!text.contains('└'), "canvas=\n{text}");

        let theme = *theme::current();
        assert_eq!(
            style_at(&canvas, "explore").color,
            Some(theme.color(ThemeColorKey::AgentBlue))
        );
        assert_eq!(
            style_at(&canvas, "@alpha").color,
            Some(theme.color(ThemeColorKey::AgentPurple))
        );
        assert_eq!(style_at(&canvas, "调研").color, Some(theme.inactive));
        assert_eq!(style_at(&canvas, "✔").color, Some(theme.success));
        assert_eq!(style_at(&canvas, "✘").color, Some(theme.error));
        assert_eq!(style_at(&canvas, "cargo test").color, Some(theme.inactive));
        assert_eq!(style_at(&canvas, "14 tools").color, Some(theme.inactive));
    }

    #[test]
    fn canvas_highlights_the_selected_row_and_expands_its_latest_calls() {
        let data = AgentPanelData {
            rows: mixed_rows(),
            terminal_rows: 30,
            selected: Some(1),
            columns: 80,
            tick: 0,
        };
        let canvas = render(data, 80);
        let text = canvas.to_string();
        let theme = *theme::current();
        assert_eq!(
            style_at(&canvas, "@alpha").background,
            Some(theme.selection_bg),
            "canvas=\n{text}"
        );
        assert_eq!(
            style_at(&canvas, "explore").background,
            None,
            "canvas=\n{text}"
        );
        // The selected row gets the `▶ ` prefix in `claude`; the others keep
        // two spaces.
        let selected_line = text.lines().find(|line| line.contains("@alpha")).unwrap();
        assert!(selected_line.starts_with("▶ "), "canvas=\n{text}");
        assert_eq!(
            style_at(&canvas, "▶").color,
            Some(theme.nyan),
            "canvas=\n{text}"
        );
        // The teammate's single call expands with the `⎿` leader.
        let expanded = text
            .lines()
            .find(|line| line.contains("Reviewing diff") && line.contains('⎿'))
            .expect("expanded call under the selected row");
        assert!(expanded.starts_with("      ⎿ "), "canvas=\n{text}");
        assert_eq!(
            style_at(&canvas, "⎿").color,
            Some(theme.inactive),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn canvas_expands_at_most_three_calls_newest_last() {
        let mut rows = mixed_rows();
        rows[0].recent_activities = (0..5).map(|i| format!("call-{i}")).collect();
        let data = AgentPanelData {
            rows,
            terminal_rows: 30,
            selected: Some(0),
            columns: 80,
            tick: 0,
        };
        let canvas = render(data, 80);
        let text = canvas.to_string();
        let expanded = text
            .lines()
            .filter(|line| line.contains("call-") || line.contains('⎿'))
            .collect::<Vec<_>>();
        assert_eq!(expanded.len(), 3, "canvas=\n{text}");
        assert!(expanded[0].contains("call-2"), "canvas=\n{text}");
        assert!(expanded[1].contains("call-3"), "canvas=\n{text}");
        assert!(expanded[2].contains("call-4"), "canvas=\n{text}");
        assert!(expanded[0].contains('⎿'), "canvas=\n{text}");
        assert!(!expanded[1].contains('⎿'), "canvas=\n{text}");
    }

    #[test]
    fn canvas_hint_line_only_when_focused_and_reads_the_bindings() {
        let unfocused = layout_agent_panel(
            &AgentPanelData {
                rows: mixed_rows(),
                terminal_rows: 30,
                selected: None,
                columns: 80,
                tick: 0,
            },
            &*theme::current(),
        );
        assert!(
            !unfocused
                .iter()
                .any(|line| line.text().contains("select ·"))
        );

        let focused = layout_agent_panel(
            &AgentPanelData {
                rows: mixed_rows(),
                terminal_rows: 30,
                selected: Some(0),
                columns: 80,
                tick: 0,
            },
            &*theme::current(),
        );
        let hint = focused.last().unwrap();
        assert_eq!(hint.prefix, "  ");
        assert!(
            hint.text()
                .contains("↑↓ select · Enter details · x stop · Esc back"),
            "{}",
            hint.text()
        );
        assert_eq!(hint.spans[0].color, (*theme::current()).inactive.into());
    }

    #[test]
    fn plan_sacrifices_expanded_lines_before_task_rows() {
        // ≥ 24 rows: 4 task rows, up to 3 expanded calls + the hint, total ≤ 8.
        let plan = plan_panel(30, 4, Some(0), 3).unwrap();
        assert_eq!(plan.task_lines, 4);
        assert_eq!(plan.expanded_lines, 3);
        assert!(plan.hint_line);
        assert_eq!(plan.total_lines(), 8);

        // 20 rows: two task rows, no expansion, still a hint line.
        let plan = plan_panel(20, 4, Some(0), 3).unwrap();
        assert_eq!(plan.task_lines, 2);
        assert_eq!(plan.hidden_rows, 2);
        assert_eq!(plan.expanded_lines, 0);
        assert!(plan.hint_line);
        assert_eq!(plan.total_lines(), 3);

        // 12 rows: hidden entirely.
        assert_eq!(plan_panel(12, 4, Some(0), 3), None);
        // No rows: hidden, so PromptInput can drop the `↓ agents` hint.
        assert_eq!(plan_panel(30, 0, None, 0), None);
    }

    #[test]
    fn plan_trims_expansion_then_task_rows_to_stay_under_the_cap() {
        // A selection with the key hint and the overflow line: 4 task lines +
        // 3 expanded + 1 hint = 8 exactly at the cap.
        let plan = plan_panel(30, 9, Some(0), 5).unwrap();
        assert_eq!(plan.task_lines, 4);
        assert_eq!(plan.expanded_lines, 3);
        assert_eq!(plan.total_lines(), 8);

        // Forcing overflow (no cap breach) keeps the overflow row.
        assert!(plan.hidden_rows > 0);
    }

    #[test]
    fn layout_keeps_every_line_inside_the_columns() {
        let long = "a very long activity that keeps going and going past any terminal edge";
        let mut rows = mixed_rows();
        rows[0].activity = long.to_string();
        rows[0].recent_activities = vec![long.to_string(); 3];
        rows[1].name = "@a-very-long-teammate-name".to_string();
        for columns in [80usize, 60, 40] {
            for tick in [0usize, 1] {
                let data = AgentPanelData {
                    rows: rows.clone(),
                    terminal_rows: 30,
                    selected: Some(0),
                    columns,
                    tick,
                };
                for line in layout_agent_panel(&data, &*theme::current()) {
                    assert!(
                        width(&line.text()) <= columns,
                        "columns={columns}: {}",
                        line.text()
                    );
                }
                for line in render(data, columns).to_string().lines() {
                    assert!(
                        width(line) <= columns,
                        "canvas line over {columns}: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn canvas_at_forty_columns_fits_the_markers_expansion_and_hint() {
        let mut rows = mixed_rows();
        rows[0].recent_activities = vec![
            "Grep \"AgentPanel\" in src/ and then some more text".into(),
            "Read src/screens/repl.rs".into(),
            "Read src/components/agent_panel.rs".into(),
        ];
        let data = AgentPanelData {
            rows,
            terminal_rows: 30,
            selected: Some(0),
            columns: 40,
            tick: 0,
        };
        let canvas = render(data, 40);
        let text = canvas.to_string();
        for line in text.lines() {
            assert!(width(line) <= 40, "line over 40 columns: {line:?}");
        }
        assert!(text.contains("▶ "), "canvas=\n{text}");
        assert!(text.contains('⎿'), "canvas=\n{text}");
        assert!(text.contains("select ·"), "canvas=\n{text}");
        // Stats are the first thing dropped at this width.
        assert!(!text.contains("14 tools"), "canvas=\n{text}");
    }

    #[test]
    fn empty_or_hidden_panel_renders_nothing() {
        let data = AgentPanelData {
            rows: Vec::new(),
            terminal_rows: 30,
            selected: None,
            columns: 80,
            tick: 0,
        };
        assert_eq!(render(data, 80).to_string().trim(), "");
        let data = AgentPanelData {
            rows: mixed_rows(),
            terminal_rows: 12,
            selected: None,
            columns: 80,
            tick: 0,
        };
        assert_eq!(render(data, 80).to_string().trim(), "");
    }

    #[derive(Default, Props)]
    struct ConnectedProbeProps {
        store: Option<crate::state::store::AppStore>,
        now_ms: Option<u64>,
        rows: Option<u32>,
    }

    #[component]
    fn ConnectedProbe(
        props: &ConnectedProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let store = props.store.clone().expect("probe store");
        let rows = props.rows;
        let now_ms = props.now_ms;
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        AgentPanel(
                            columns: Some(80u32),
                            rows: rows,
                            now_ms: now_ms,
                            tick: Some(0usize),
                        )
                    }.into_any()),
                )
            }
        }
    }

    fn connected_store(tasks: Vec<TaskState>) -> crate::state::store::AppStore {
        let mut state = crate::state::app_state_store::AppState::default();
        state.tasks = Arc::new(tasks_map(tasks));
        crate::state::store::AppStore::new(state, None)
    }

    #[test]
    fn connected_panel_reads_tasks_and_publishes_displayed_rows() {
        let store = connected_store(vec![
            TaskState::LocalShell(shell("s1", "cargo test", "running", 0)),
            TaskState::InProcessTeammate(snapshot("t1", "alpha", "running")),
        ]);
        let canvas = element!(ConnectedProbe(
            store: Some(store.clone()),
            now_ms: Some(1_000u64),
            rows: Some(30u32),
        ))
        .render(Some(80));
        let text = canvas.to_string();
        assert!(text.contains("cargo test"), "canvas=\n{text}");
        assert!(text.contains('$'), "canvas=\n{text}");
        assert!(text.contains("@alpha"), "canvas=\n{text}");
        let state = store.get();
        assert_eq!(
            state
                .agent_panel_items
                .iter()
                .map(|item| (item.id.as_str(), item.kind))
                .collect::<Vec<_>>(),
            vec![("s1", AgentKind::Shell), ("t1", AgentKind::Teammate)]
        );
        assert_eq!(
            state.agent_panel_finished_hint_until_ms, None,
            "running rows arm no hint"
        );
    }

    #[test]
    fn connected_panel_hides_below_sixteen_rows_and_empty_footer_items() {
        let store = connected_store(vec![TaskState::LocalShell(shell(
            "s1",
            "cargo test",
            "running",
            0,
        ))]);
        let canvas = element!(ConnectedProbe(
            store: Some(store.clone()),
            now_ms: Some(1_000u64),
            rows: Some(12u32),
        ))
        .render(Some(80));
        assert_eq!(canvas.to_string().trim(), "");
        assert!(store.get().agent_panel_items.is_empty());
    }

    #[test]
    fn connected_panel_clamps_a_stale_selection() {
        let mut state = crate::state::app_state_store::AppState::default();
        state.tasks = Arc::new(tasks_map(vec![TaskState::LocalShell(shell(
            "s1",
            "cargo test",
            "running",
            0,
        ))]));
        state.agent_panel_selected = 5;
        let store = crate::state::store::AppStore::new(state, None);
        element!(ConnectedProbe(
            store: Some(store.clone()),
            now_ms: Some(1_000u64),
            rows: Some(30u32),
        ))
        .render(Some(80));
        assert_eq!(store.get().agent_panel_selected, 0);
    }

    #[test]
    fn connected_panel_records_the_finished_hint_on_a_completed_transition() {
        // The first mount sees the row running; the store then flips it to
        // completed, and the panel writes the 30 s hint deadline.
        let mut state = crate::state::app_state_store::AppState::default();
        state.tasks = Arc::new(tasks_map(vec![TaskState::LocalShell(shell(
            "s1",
            "cargo test",
            "running",
            0,
        ))]));
        let store = crate::state::store::AppStore::new(state, None);
        element!(ConnectedProbe(
            store: Some(store.clone()),
            now_ms: Some(1_000u64),
            rows: Some(30u32),
        ))
        .render(Some(80));
        store.set_state(|prev| {
            let mut next = (**prev).clone();
            let mut task = (**next.tasks.get("s1").unwrap()).clone();
            if let TaskState::LocalShell(shell) = &mut task {
                shell.status = "completed".to_string();
                shell.end_time_ms = Some(4_000);
            }
            Arc::make_mut(&mut next.tasks).insert("s1".to_string(), Arc::new(task));
            crate::state::store::UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            }
        });
        let canvas = element!(ConnectedProbe(
            store: Some(store.clone()),
            now_ms: Some(4_000u64),
            rows: Some(30u32),
        ))
        .render(Some(80));
        assert!(!canvas.to_string().contains("cargo test"), "row removed");
        let state = store.get();
        assert!(state.agent_panel_items.is_empty());
        assert_eq!(
            state.agent_panel_finished_hint_until_ms,
            Some(4_000 + FINISHED_HINT_MS)
        );
    }
}
