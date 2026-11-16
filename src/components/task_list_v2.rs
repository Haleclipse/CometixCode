//! @nyan: built-in todo panel (docs/nyan/tui.md §4, §7).
//!
//! Reworked from CC `components/TaskListV2.tsx`, which was never mounted in
//! production. Only v2 tasks (TaskCreate / TaskUpdate, on disk) are shown.
//! `TodoPanelView` is the presentational half (pure data in, rows out) and
//! `TodoPanel` is the AppState-connected half that mounts in `screens/repl.rs`.

use crate::constants::figures;
use crate::state::app_state::use_app_state;
use crate::state::app_state_store::ExpandedView;
use crate::utils::tasks::{TaskRecord, get_task_list_id, is_todo_v2_enabled, list_tasks};
use crate::utils::theme::Theme;
use crate::utils::truncate::truncate_to_width;
use iocraft::prelude::*;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::UnicodeWidthStr;

/// Entry rows allowed at ≥ 24 terminal rows (§7).
pub const MAX_ENTRY_ROWS_TALL: usize = 6;
/// Entry rows allowed at 16–23 terminal rows (§7).
pub const MAX_ENTRY_ROWS_MEDIUM: usize = 3;
/// Below this many terminal rows the panel is forced to its one-line form.
pub const MIN_ROWS_FOR_EXPANDED: usize = 16;
const TALL_ROWS: usize = 24;
/// A title squeezed below this width gives up its right-hand text instead.
const MIN_TITLE_WIDTH: usize = 8;
const SPINNER_FIRST_PREFIX: &str = "  ⎿  ";
const SPINNER_REST_PREFIX: &str = "     ";
const IDLE_PREFIX: &str = "  ";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskStatusV2 {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskV2 {
    pub id: String,
    pub subject: String,
    pub status: TaskStatusV2,
    pub owner: Option<String>,
    pub blocked_by: Vec<String>,
}

/// Disk records to panel tasks. Only `pending` / `in_progress` / `completed`
/// are shown; `_internal` tasks are hidden like the TaskList tool hides them.
pub fn tasks_from_records(records: Vec<TaskRecord>) -> Vec<TaskV2> {
    records
        .into_iter()
        .filter_map(|record| {
            let internal = record
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("_internal"))
                .is_some_and(|value| {
                    !matches!(
                        value,
                        serde_json::Value::Null | serde_json::Value::Bool(false)
                    )
                });
            if internal {
                return None;
            }
            let status = match record.status.as_str() {
                "pending" => TaskStatusV2::Pending,
                "in_progress" => TaskStatusV2::InProgress,
                "completed" => TaskStatusV2::Completed,
                _ => return None,
            };
            Some(TaskV2 {
                id: record.id,
                subject: record.subject,
                status,
                owner: record.owner,
                blocked_by: record.blocked_by,
            })
        })
        .collect()
}

/// Maps to: CC `TaskListV2.tsx#byIdAsc`.
pub fn task_list_v2_by_id_asc(a: &TaskV2, b: &TaskV2) -> std::cmp::Ordering {
    match (a.id.parse::<i64>(), b.id.parse::<i64>()) {
        (Ok(a_num), Ok(b_num)) => a_num.cmp(&b_num),
        _ => a.id.cmp(&b.id),
    }
}

fn sorted_by_id(tasks: &[TaskV2]) -> Vec<TaskV2> {
    let mut sorted = tasks.to_vec();
    sorted.sort_by(task_list_v2_by_id_asc);
    sorted
}

/// Completed count and total.
pub fn progress(tasks: &[TaskV2]) -> (usize, usize) {
    let completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatusV2::Completed)
        .count();
    (completed, tasks.len())
}

/// True for a non-empty list whose every task is completed.
pub fn all_tasks_done(tasks: &[TaskV2]) -> bool {
    !tasks.is_empty()
        && tasks
            .iter()
            .all(|task| task.status == TaskStatusV2::Completed)
}

/// Hash of every task's id and status. Any create, delete or status change
/// yields a different value; subject / owner edits do not.
pub fn task_signature(tasks: &[TaskV2]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for task in sorted_by_id(tasks) {
        task.id.hash(&mut hasher);
        (task.status as u8).hash(&mut hasher);
    }
    hasher.finish()
}

/// The signature to record when a user turn starts: `Some` only if the list is
/// entirely done, so the finished block disappears after the next submit.
pub fn cleared_signature_for_turn(tasks: &[TaskV2]) -> Option<u64> {
    all_tasks_done(tasks).then(|| task_signature(tasks))
}

/// Read the current list from disk, as the panel shows it.
pub fn load_panel_tasks() -> Vec<TaskV2> {
    tasks_from_records(list_tasks(&get_task_list_id()))
}

/// A pending task is blocked while any of its `blocked_by` ids is unfinished.
pub fn is_blocked(task: &TaskV2, unresolved_ids: &HashSet<String>) -> bool {
    task.status == TaskStatusV2::Pending
        && task.blocked_by.iter().any(|id| unresolved_ids.contains(id))
}

/// Entry-row budget from terminal rows (§7). `None` means "force one line".
/// `rows == 0` is an unknown size and is treated as tall.
pub fn max_entry_rows(rows: usize) -> Option<usize> {
    let rows = if rows == 0 { usize::MAX } else { rows };
    if rows >= TALL_ROWS {
        Some(MAX_ENTRY_ROWS_TALL)
    } else if rows >= MIN_ROWS_FOR_EXPANDED {
        Some(MAX_ENTRY_ROWS_MEDIUM)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelEntry {
    /// `✔ <last completed>  +N`
    Completed {
        title: String,
        more: usize,
    },
    Task {
        task: TaskV2,
        blocked: bool,
    },
    /// `… +N pending`
    Overflow {
        hidden: usize,
    },
}

/// Completed (folded) → in progress → pending, each group by id. At most
/// `max_rows` rows; when more would be needed the last row becomes
/// `… +N pending` and covers every row it replaced.
pub fn panel_entries(tasks: &[TaskV2], max_rows: usize) -> Vec<PanelEntry> {
    let sorted = sorted_by_id(tasks);
    let unresolved = sorted
        .iter()
        .filter(|task| task.status != TaskStatusV2::Completed)
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let completed = sorted
        .iter()
        .filter(|task| task.status == TaskStatusV2::Completed)
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    if let Some(last) = completed.last() {
        entries.push(PanelEntry::Completed {
            title: last.subject.clone(),
            more: completed.len() - 1,
        });
    }
    for status in [TaskStatusV2::InProgress, TaskStatusV2::Pending] {
        for task in sorted.iter().filter(|task| task.status == status) {
            entries.push(PanelEntry::Task {
                blocked: is_blocked(task, &unresolved),
                task: task.clone(),
            });
        }
    }
    let max_rows = max_rows.max(2);
    if entries.len() > max_rows {
        let hidden = entries.len() - (max_rows - 1);
        entries.truncate(max_rows - 1);
        entries.push(PanelEntry::Overflow { hidden });
    }
    entries
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelForm {
    Hidden,
    AllDone,
    Collapsed,
    Expanded,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelData {
    pub tasks: Vec<TaskV2>,
    /// `expanded_view == Tasks`.
    pub expanded: bool,
    /// A spinner row is above the panel: entries hang off `⎿`.
    pub spinner_active: bool,
    /// 0 reads as 80.
    pub columns: usize,
    /// Terminal rows; 0 means unknown.
    pub rows: usize,
    pub cleared_signature: Option<u64>,
}

pub fn panel_form(data: &PanelData) -> PanelForm {
    if data.tasks.is_empty() {
        return PanelForm::Hidden;
    }
    if all_tasks_done(&data.tasks) {
        return if data.cleared_signature == Some(task_signature(&data.tasks)) {
            PanelForm::Hidden
        } else {
            PanelForm::AllDone
        };
    }
    if data.expanded && max_entry_rows(data.rows).is_some() {
        PanelForm::Expanded
    } else {
        PanelForm::Collapsed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub color: Option<Color>,
    pub bold: bool,
    pub strike: bool,
}

impl Span {
    fn plain(text: impl Into<String>, color: Option<Color>) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
            strike: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelLine {
    pub prefix: String,
    pub prefix_color: Option<Color>,
    pub spans: Vec<Span>,
    pub right: Option<Span>,
}

impl PanelLine {
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

/// The bound key for the todo panel toggle, so a user remap is reflected in
/// the panel's hint (same lookup as `screens/repl.rs`'s ctrl+o hint).
pub fn toggle_todos_shortcut() -> String {
    crate::keybindings::shortcut_format::get_shortcut_display(
        "app:toggleTodos",
        &crate::keybindings::types::ContextName::Global,
        "ctrl+t",
    )
}

/// Truncate `title` so the line fits `columns`. `fixed` is everything on the
/// line except the title and the right-hand text; the right-hand text keeps a
/// two-column gap and is dropped when it would squeeze the title too far.
fn fit_title(title: &str, columns: usize, fixed: usize, right_width: usize) -> (String, bool) {
    if right_width > 0 {
        let budget = columns.saturating_sub(fixed + right_width + 2);
        if budget >= MIN_TITLE_WIDTH || width(title) <= budget {
            return (truncate_to_width(title, budget.max(1)), true);
        }
    }
    (
        truncate_to_width(title, columns.saturating_sub(fixed).max(1)),
        false,
    )
}

fn entry_line(
    entry: &PanelEntry,
    prefix: &str,
    right: Option<&str>,
    columns: usize,
    theme: &Theme,
) -> PanelLine {
    let fig = figures::get();
    let inactive = Some(theme.inactive);
    let right_width = right.map_or(0, width);
    let (icon, icon_color, title, title_color, bold, strike, suffix) = match entry {
        PanelEntry::Completed { title, more } => (
            fig.tick,
            Some(theme.success),
            title.as_str(),
            inactive,
            false,
            true,
            if *more > 0 {
                vec![Span::plain(format!("  +{more}"), inactive)]
            } else {
                Vec::new()
            },
        ),
        PanelEntry::Task { task, blocked } => {
            let mut suffix = Vec::new();
            if let Some(owner) = task.owner.as_deref().filter(|owner| !owner.is_empty()) {
                suffix.push(Span::plain(format!(" @{owner}"), inactive));
            }
            if *blocked {
                suffix.push(Span::plain(" · blocked", inactive));
            }
            if task.status == TaskStatusV2::InProgress {
                (
                    fig.square_small_filled,
                    Some(theme.nyan),
                    task.subject.as_str(),
                    Some(theme.nyan),
                    true,
                    false,
                    suffix,
                )
            } else {
                (
                    fig.square_small,
                    inactive,
                    task.subject.as_str(),
                    inactive,
                    false,
                    false,
                    suffix,
                )
            }
        }
        PanelEntry::Overflow { hidden } => {
            return PanelLine {
                prefix: prefix.to_string(),
                prefix_color: inactive,
                spans: vec![Span::plain(format!("… +{hidden} pending"), inactive)],
                right: right.map(|text| Span::plain(text, inactive)),
            };
        }
    };
    let icon_text = format!("{icon} ");
    let suffix_width = suffix.iter().map(|span| width(&span.text)).sum::<usize>();
    let fixed = width(prefix) + width(&icon_text) + suffix_width;
    let (title, keep_right) = fit_title(title, columns, fixed, right_width);
    let mut spans = vec![
        Span {
            text: icon_text,
            color: icon_color,
            bold,
            strike: false,
        },
        Span {
            text: title,
            color: title_color,
            bold,
            strike,
        },
    ];
    spans.extend(suffix);
    PanelLine {
        prefix: prefix.to_string(),
        prefix_color: inactive,
        spans,
        right: right
            .filter(|_| keep_right)
            .map(|text| Span::plain(text, inactive)),
    }
}

/// Rows of the panel for `data`, top to bottom. Pure; the component only
/// paints them.
pub fn layout_panel(data: &PanelData, theme: &Theme) -> Vec<PanelLine> {
    let columns = if data.columns == 0 { 80 } else { data.columns };
    let inactive = Some(theme.inactive);
    let (done, total) = progress(&data.tasks);
    let progress_text = format!("{done}/{total}");
    match panel_form(data) {
        PanelForm::Hidden => Vec::new(),
        PanelForm::AllDone => vec![PanelLine {
            prefix: IDLE_PREFIX.to_string(),
            prefix_color: None,
            spans: vec![Span::plain(
                format!("{} {total}/{total} done", figures::get().tick),
                Some(theme.success),
            )],
            right: None,
        }],
        PanelForm::Collapsed => {
            let current = sorted_by_id(&data.tasks)
                .into_iter()
                .find(|task| task.status == TaskStatusV2::InProgress)
                .or_else(|| {
                    sorted_by_id(&data.tasks)
                        .into_iter()
                        .find(|task| task.status == TaskStatusV2::Pending)
                });
            let Some(task) = current else {
                return Vec::new();
            };
            let fig = figures::get();
            let in_progress = task.status == TaskStatusV2::InProgress;
            let color = if in_progress {
                Some(theme.nyan)
            } else {
                inactive
            };
            let icon = if in_progress {
                fig.square_small_filled
            } else {
                fig.square_small
            };
            let icon_text = format!("{icon} ");
            let tail = format!(" · {progress_text}");
            let shortcut = toggle_todos_shortcut();
            let right = shortcut.as_str();
            let fixed = width(SPINNER_FIRST_PREFIX) + width(&icon_text) + width(&tail);
            let (title, keep_right) = fit_title(&task.subject, columns, fixed, width(right));
            vec![PanelLine {
                prefix: SPINNER_FIRST_PREFIX.to_string(),
                prefix_color: inactive,
                spans: vec![
                    Span {
                        text: icon_text,
                        color,
                        bold: in_progress,
                        strike: false,
                    },
                    Span {
                        text: title,
                        color,
                        bold: in_progress,
                        strike: false,
                    },
                    Span::plain(tail, inactive),
                ],
                right: keep_right.then(|| Span::plain(right, inactive)),
            }]
        }
        PanelForm::Expanded => {
            let max_rows = max_entry_rows(data.rows).unwrap_or(MAX_ENTRY_ROWS_TALL);
            let entries = panel_entries(&data.tasks, max_rows);
            let last = entries.len().saturating_sub(1);
            if data.spinner_active {
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        let prefix = if index == 0 {
                            SPINNER_FIRST_PREFIX
                        } else {
                            SPINNER_REST_PREFIX
                        };
                        let right = (index == last).then_some(progress_text.as_str());
                        entry_line(entry, prefix, right, columns, theme)
                    })
                    .collect()
            } else {
                let mut lines = vec![PanelLine {
                    prefix: IDLE_PREFIX.to_string(),
                    prefix_color: None,
                    spans: vec![
                        Span::plain("Tasks", None),
                        Span::plain(format!(" {progress_text}"), inactive),
                    ],
                    right: Some(Span::plain(
                        format!("{} to collapse", toggle_todos_shortcut()),
                        inactive,
                    )),
                }];
                lines.extend(
                    entries
                        .iter()
                        .map(|entry| entry_line(entry, IDLE_PREFIX, None, columns, theme)),
                );
                lines
            }
        }
    }
}

fn span_element(span: &Span) -> AnyElement<'static> {
    element! {
        Text(
            content: span.text.clone(),
            color: span.color,
            weight: if span.bold { Weight::Bold } else { Weight::Normal },
            strikethrough: span.strike,
            wrap: TextWrap::NoWrap,
        )
    }
    .into_any()
}

fn line_element(line: &PanelLine) -> AnyElement<'static> {
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
        ) {
            #(cells)
        }
    }
    .into_any()
}

#[derive(Default, Props)]
pub struct TodoPanelViewProps {
    pub data: PanelData,
}

/// Presentational half: paints `layout_panel` and nothing else.
#[component]
pub fn TodoPanelView(props: &TodoPanelViewProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let lines = layout_panel(&props.data, &theme);
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
pub struct TodoPanelProps {
    /// The spinner row is mounted right above this panel.
    pub spinner_active: bool,
    /// Override the terminal size (tests); `None` reads the terminal.
    pub columns: Option<u32>,
    pub rows: Option<u32>,
}

struct TaskCache {
    revision: u64,
    task_list_id: String,
    tasks: Vec<TaskV2>,
}

/// AppState-connected half. The list is re-read from disk only when
/// `task_list_revision` or the task list id changes, never per frame.
/// The id changes on `/clear` and `/resume` (it falls back to the session
/// id), which bump no revision.
#[component]
pub fn TodoPanel(props: &TodoPanelProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (terminal_columns, terminal_rows) = hooks.use_terminal_size();
    let expanded = use_app_state(&mut hooks, |state| {
        state.expanded_view == ExpandedView::Tasks
    });
    let revision = use_app_state(&mut hooks, |state| state.task_list_revision);
    let cleared_signature = use_app_state(&mut hooks, |state| state.todo_cleared_signature);
    let mut cache = hooks.use_ref(|| TaskCache {
        revision: u64::MAX,
        task_list_id: String::new(),
        tasks: Vec::new(),
    });
    if !is_todo_v2_enabled() {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }
    let task_list_id = get_task_list_id();
    let stale = {
        let cached = cache.read();
        cached.revision != revision || cached.task_list_id != task_list_id
    };
    if stale {
        let tasks = tasks_from_records(list_tasks(&task_list_id));
        *cache.write() = TaskCache {
            revision,
            task_list_id,
            tasks,
        };
    }
    let tasks = cache.read().tasks.clone();
    let data = PanelData {
        tasks,
        expanded,
        spinner_active: props.spinner_active,
        columns: props.columns.unwrap_or(terminal_columns as u32) as usize,
        rows: props.rows.unwrap_or(terminal_rows as u32) as usize,
        cleared_signature,
    };
    element! { TodoPanelView(data: data) }.into_any()
}

/// Called next to `footer_metrics::start_user_turn`: if every task is done,
/// remember its signature so the "N/N done" line disappears with this turn.
/// Reads the list from disk once per turn start, never per frame.
pub fn dismiss_finished_tasks(store: &crate::state::store::AppStore) {
    if !is_todo_v2_enabled() {
        return;
    }
    let signature = cleared_signature_for_turn(&load_panel_tasks());
    if signature.is_some() && store.get().todo_cleared_signature != signature {
        store.replace_with(|state| state.todo_cleared_signature = signature);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn task(id: &str, status: TaskStatusV2) -> TaskV2 {
        TaskV2 {
            id: id.to_string(),
            subject: format!("task {id}"),
            status,
            owner: None,
            blocked_by: Vec::new(),
        }
    }

    fn titled(id: &str, status: TaskStatusV2, subject: &str) -> TaskV2 {
        TaskV2 {
            subject: subject.to_string(),
            ..task(id, status)
        }
    }

    fn data(tasks: Vec<TaskV2>, expanded: bool, spinner_active: bool, columns: usize) -> PanelData {
        PanelData {
            tasks,
            expanded,
            spinner_active,
            columns,
            rows: 30,
            cleared_signature: None,
        }
    }

    fn render(data: PanelData, width: usize) -> Canvas {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                TodoPanelView(data: data)
            }
        }
        .render(Some(width))
    }

    struct CellStyle {
        color: Option<Color>,
        bold: bool,
        strike: bool,
    }

    /// Style of the first cell of `needle` on the first line that contains it.
    fn style_at(canvas: &Canvas, needle: &str) -> CellStyle {
        let text = canvas.to_string();
        let line_index = text
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in canvas=\n{text}"));
        let line = text.lines().nth(line_index).unwrap();
        let x = UnicodeWidthStr::width(&line[..line.find(needle).unwrap()]);
        let style = canvas
            .resolved_text_style(x, line_index)
            .unwrap_or_else(|| panic!("no style at {needle:?} in canvas=\n{text}"));
        CellStyle {
            color: style.color,
            bold: style.weight == Weight::Bold,
            strike: style.strikethrough,
        }
    }

    fn mixed() -> Vec<TaskV2> {
        vec![
            titled("1", TaskStatusV2::Completed, "Survey"),
            titled("2", TaskStatusV2::Completed, "Draft"),
            titled("3", TaskStatusV2::Completed, "Review"),
            titled("4", TaskStatusV2::InProgress, "Build footer"),
            titled("5", TaskStatusV2::Pending, "Drop statusLine"),
            titled("6", TaskStatusV2::Pending, "Narrow screens"),
            titled("7", TaskStatusV2::Pending, "Polish"),
        ]
    }

    #[test]
    fn entries_sort_completed_then_in_progress_then_pending_by_numeric_id() {
        let tasks = vec![
            task("10", TaskStatusV2::Pending),
            task("2", TaskStatusV2::Pending),
            task("11", TaskStatusV2::InProgress),
            task("3", TaskStatusV2::Completed),
        ];
        let ids = panel_entries(&tasks, 6)
            .into_iter()
            .map(|entry| match entry {
                PanelEntry::Completed { title, .. } => format!("done:{title}"),
                PanelEntry::Task { task, .. } => task.id,
                PanelEntry::Overflow { .. } => "overflow".to_string(),
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["done:task 3", "11", "2", "10"]);
    }

    #[test]
    fn completed_fold_shows_latest_and_counts_the_rest() {
        let entries = panel_entries(&mixed(), 6);
        assert_eq!(
            entries[0],
            PanelEntry::Completed {
                title: "Review".to_string(),
                more: 2
            }
        );
        let one = panel_entries(
            &[
                task("1", TaskStatusV2::Completed),
                task("2", TaskStatusV2::Pending),
            ],
            6,
        );
        assert_eq!(
            one[0],
            PanelEntry::Completed {
                title: "task 1".to_string(),
                more: 0
            }
        );
    }

    #[test]
    fn blocked_means_an_unfinished_blocker_remains() {
        let tasks = vec![
            task("1", TaskStatusV2::Completed),
            task("2", TaskStatusV2::InProgress),
            TaskV2 {
                blocked_by: vec!["2".into()],
                ..task("3", TaskStatusV2::Pending)
            },
            TaskV2 {
                blocked_by: vec!["1".into()],
                ..task("4", TaskStatusV2::Pending)
            },
            TaskV2 {
                blocked_by: vec!["1".into(), "3".into()],
                ..task("5", TaskStatusV2::Pending)
            },
        ];
        let blocked = panel_entries(&tasks, 6)
            .into_iter()
            .filter_map(|entry| match entry {
                PanelEntry::Task { task, blocked } => Some((task.id, blocked)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            blocked,
            vec![
                ("2".to_string(), false),
                ("3".to_string(), true),
                ("4".to_string(), false),
                ("5".to_string(), true),
            ]
        );
    }

    #[test]
    fn overflow_row_replaces_the_tail_and_counts_everything_hidden() {
        let mut tasks = vec![task("1", TaskStatusV2::InProgress)];
        tasks.extend((2..=10).map(|id| task(&id.to_string(), TaskStatusV2::Pending)));
        let entries = panel_entries(&tasks, 6);
        assert_eq!(entries.len(), 6);
        // 10 tasks, 5 shown, 5 hidden.
        assert_eq!(entries[5], PanelEntry::Overflow { hidden: 5 });

        let exact = panel_entries(&tasks[..6], 6);
        assert_eq!(exact.len(), 6);
        assert!(
            !exact
                .iter()
                .any(|e| matches!(e, PanelEntry::Overflow { .. }))
        );
    }

    #[test]
    fn records_filter_to_the_three_visible_statuses_and_internal_tasks() {
        let record = |id: &str, status: &str, internal: bool| TaskRecord {
            id: id.to_string(),
            subject: format!("s{id}"),
            description: String::new(),
            active_form: None,
            status: status.to_string(),
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: internal.then(|| {
                serde_json::json!({"_internal": true})
                    .as_object()
                    .unwrap()
                    .clone()
            }),
        };
        let tasks = tasks_from_records(vec![
            record("1", "pending", false),
            record("2", "in_progress", false),
            record("3", "completed", false),
            record("4", "weird", false),
            record("5", "pending", true),
        ]);
        assert_eq!(
            tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn signature_tracks_ids_and_statuses_only() {
        let done = vec![
            task("1", TaskStatusV2::Completed),
            task("2", TaskStatusV2::Completed),
        ];
        let signature = task_signature(&done);
        let renamed = vec![
            titled("1", TaskStatusV2::Completed, "other"),
            task("2", TaskStatusV2::Completed),
        ];
        assert_eq!(task_signature(&renamed), signature);
        let reordered = vec![done[1].clone(), done[0].clone()];
        assert_eq!(task_signature(&reordered), signature);
        let added = vec![
            done[0].clone(),
            done[1].clone(),
            task("3", TaskStatusV2::Pending),
        ];
        assert_ne!(task_signature(&added), signature);
        let reopened = vec![done[0].clone(), task("2", TaskStatusV2::InProgress)];
        assert_ne!(task_signature(&reopened), signature);
    }

    #[test]
    fn clearing_applies_only_to_a_fully_done_list_and_expires_on_any_change() {
        assert_eq!(cleared_signature_for_turn(&[]), None);
        assert_eq!(cleared_signature_for_turn(&mixed()), None);
        let done = vec![
            task("1", TaskStatusV2::Completed),
            task("2", TaskStatusV2::Completed),
        ];
        let signature = cleared_signature_for_turn(&done);
        assert!(signature.is_some());

        let mut panel = data(done.clone(), false, false, 80);
        assert_eq!(panel_form(&panel), PanelForm::AllDone);
        panel.cleared_signature = signature;
        assert_eq!(panel_form(&panel), PanelForm::Hidden);

        panel.tasks.push(task("3", TaskStatusV2::Pending));
        assert_eq!(panel_form(&panel), PanelForm::Collapsed);
        panel.tasks[2].status = TaskStatusV2::Completed;
        assert_eq!(
            panel_form(&panel),
            PanelForm::AllDone,
            "new signature, shows again"
        );
    }

    #[test]
    fn height_tiers_for_30_20_and_12_rows() {
        assert_eq!(max_entry_rows(30), Some(6));
        assert_eq!(max_entry_rows(24), Some(6));
        assert_eq!(max_entry_rows(23), Some(3));
        assert_eq!(max_entry_rows(20), Some(3));
        assert_eq!(max_entry_rows(16), Some(3));
        assert_eq!(max_entry_rows(15), None);
        assert_eq!(max_entry_rows(12), None);
        assert_eq!(
            max_entry_rows(0),
            Some(6),
            "unknown size is treated as tall"
        );

        let theme = *theme::current();
        let rows = |terminal_rows: usize| {
            let mut panel = data(mixed(), true, false, 80);
            panel.rows = terminal_rows;
            layout_panel(&panel, &theme)
        };
        // Title row + entries.
        assert_eq!(rows(30).len(), 1 + 5);
        assert_eq!(rows(20).len(), 1 + 3);
        let short = rows(12);
        assert_eq!(short.len(), 1, "forced to the one-line form");
        assert!(short[0].text().contains(&toggle_todos_shortcut()));
    }

    #[test]
    fn forced_collapse_does_not_depend_on_expanded_view_being_changed() {
        let mut panel = data(mixed(), true, true, 80);
        panel.rows = 12;
        assert_eq!(panel_form(&panel), PanelForm::Collapsed);
        assert!(panel.expanded, "the input flag is untouched");
        panel.rows = 30;
        assert_eq!(panel_form(&panel), PanelForm::Expanded);
    }

    #[test]
    fn spinner_expanded_hangs_off_the_gutter_and_puts_progress_on_the_last_row() {
        let canvas = render(data(mixed(), true, true, 80), 80);
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[0].starts_with("  ⎿  ✔ Review  +2"), "canvas=\n{text}");
        assert!(
            lines[1].starts_with("     ◼ Build footer"),
            "canvas=\n{text}"
        );
        assert!(
            lines[2].starts_with("     ◻ Drop statusLine"),
            "canvas=\n{text}"
        );
        assert!(lines[4].starts_with("     ◻ Polish"), "canvas=\n{text}");
        assert!(lines[4].trim_end().ends_with("3/7"), "canvas=\n{text}");
        assert!(!lines[0].contains("3/7"), "canvas=\n{text}");
    }

    #[test]
    fn idle_expanded_has_a_title_row_with_progress_and_the_collapse_hint() {
        let canvas = render(data(mixed(), true, false, 80), 80);
        let text = canvas.to_string();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines[0].starts_with("  Tasks 3/7"), "canvas=\n{text}");
        assert!(
            lines[0]
                .trim_end()
                .ends_with(&format!("{} to collapse", toggle_todos_shortcut())),
            "canvas=\n{text}"
        );
        assert!(lines[1].starts_with("  ✔ Review  +2"), "canvas=\n{text}");
        assert!(lines[2].starts_with("  ◼ Build footer"), "canvas=\n{text}");
        assert!(!text.contains('⎿'), "canvas=\n{text}");
        let theme = *theme::current();
        assert_eq!(
            style_at(&canvas, &format!("{} to collapse", toggle_todos_shortcut())).color,
            Some(theme.inactive)
        );
        assert_eq!(style_at(&canvas, "3/7").color, Some(theme.inactive));
    }

    #[test]
    fn collapsed_is_one_line_with_the_current_task_and_hint() {
        let canvas = render(data(mixed(), false, true, 80), 80);
        let text = canvas.to_string();
        assert_eq!(text.lines().count(), 1, "canvas=\n{text}");
        assert!(
            text.starts_with("  ⎿  ◼ Build footer · 3/7"),
            "canvas=\n{text}"
        );
        assert!(
            text.trim_end().ends_with(&toggle_todos_shortcut()),
            "canvas=\n{text}"
        );
        let theme = *theme::current();
        assert_eq!(
            style_at(&canvas, &toggle_todos_shortcut()).color,
            Some(theme.inactive)
        );
        assert_eq!(style_at(&canvas, "◼").color, Some(theme.nyan));

        let pending_only = vec![
            titled("1", TaskStatusV2::Completed, "Done"),
            titled("2", TaskStatusV2::Pending, "Next up"),
            titled("3", TaskStatusV2::Pending, "Later"),
        ];
        let text = render(data(pending_only, false, false, 80), 80).to_string();
        assert!(text.starts_with("  ⎿  ◻ Next up · 1/3"), "canvas=\n{text}");
    }

    #[test]
    fn all_done_is_a_single_success_line() {
        let done = vec![
            task("1", TaskStatusV2::Completed),
            task("2", TaskStatusV2::Completed),
        ];
        let canvas = render(data(done, true, false, 80), 80);
        let text = canvas.to_string();
        assert_eq!(text.trim_end(), "  ✔ 2/2 done");
        assert_eq!(style_at(&canvas, "✔").color, Some(theme::current().success));
    }

    #[test]
    fn empty_list_renders_nothing() {
        assert_eq!(
            render(data(Vec::new(), true, true, 80), 80)
                .to_string()
                .trim(),
            ""
        );
    }

    #[test]
    fn row_styles_in_progress_bold_completed_struck_owner_and_blocked() {
        let tasks = vec![
            titled("1", TaskStatusV2::Completed, "Shipped"),
            TaskV2 {
                owner: Some("alice".into()),
                ..titled("2", TaskStatusV2::InProgress, "Writing")
            },
            TaskV2 {
                blocked_by: vec!["2".into()],
                ..titled("3", TaskStatusV2::Pending, "Waiting")
            },
        ];
        let canvas = render(data(tasks, true, false, 80), 80);
        let text = canvas.to_string();
        let theme = *theme::current();

        let done = style_at(&canvas, "Shipped");
        assert!(done.strike, "canvas=\n{text}");
        assert_eq!(done.color, Some(theme.inactive));
        assert_eq!(style_at(&canvas, "✔").color, Some(theme.success));

        let running = style_at(&canvas, "Writing");
        assert!(running.bold && !running.strike, "canvas=\n{text}");
        assert_eq!(running.color, Some(theme.nyan));
        assert!(text.contains("Writing @alice"), "canvas=\n{text}");
        assert_eq!(style_at(&canvas, "@alice").color, Some(theme.inactive));

        let waiting = style_at(&canvas, "Waiting");
        assert!(!waiting.bold && !waiting.strike);
        assert_eq!(waiting.color, Some(theme.inactive));
        assert!(text.contains("Waiting · blocked"), "canvas=\n{text}");
        assert_eq!(style_at(&canvas, "◻").color, Some(theme.inactive));
    }

    #[test]
    fn overflow_row_reads_plus_n_pending() {
        let mut tasks = vec![task("1", TaskStatusV2::InProgress)];
        tasks.extend((2..=9).map(|id| task(&id.to_string(), TaskStatusV2::Pending)));
        let text = render(data(tasks, true, false, 80), 80).to_string();
        assert!(text.contains("… +4 pending"), "canvas=\n{text}");
    }

    fn write_task(list_id: &str, subject: &str, status: &str) -> String {
        crate::utils::tasks::create_task(
            list_id,
            crate::utils::tasks::NewTaskData {
                subject: subject.to_string(),
                description: String::new(),
                active_form: None,
                status: status.to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
        )
        .unwrap()
    }

    fn render_panel(
        store: crate::state::store::AppStore,
        spinner_active: bool,
        rows: u32,
    ) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                crate::state::app_state::AppStateProvider(
                    prebuilt_store: Some(store),
                    children: crate::state::app_state::ProviderChildren::new(move || element! {
                        TodoPanel(spinner_active: spinner_active, columns: Some(80u32), rows: Some(rows))
                    }.into_any()),
                )
            }
        }
        .render(Some(80))
        .to_string()
    }

    #[test]
    fn connected_panel_reads_the_disk_list_and_follows_expanded_view() {
        use crate::state::app_state_store::{AppState, ExpandedView};
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("panel-connected");
        let list_id = crate::utils::tasks::get_task_list_id();
        write_task(&list_id, "Finished one", "completed");
        write_task(&list_id, "Doing it", "in_progress");
        write_task(&list_id, "Later", "pending");

        let mut expanded = AppState::default();
        expanded.expanded_view = ExpandedView::Tasks;
        let text = render_panel(crate::state::store::AppStore::new(expanded, None), true, 30);
        assert!(text.contains("  ⎿  ✔ Finished one"), "canvas=\n{text}");
        assert!(text.contains("◼ Doing it"), "canvas=\n{text}");
        assert!(text.contains("◻ Later"), "canvas=\n{text}");
        assert!(text.contains("1/3"), "canvas=\n{text}");

        let collapsed = crate::state::store::AppStore::new(AppState::default(), None);
        let text = render_panel(collapsed.clone(), true, 30);
        assert_eq!(text.lines().count(), 1, "canvas=\n{text}");
        assert!(text.contains("◼ Doing it · 1/3"), "canvas=\n{text}");

        // Forced one-line form on a short terminal, expanded_view untouched.
        let short_store = {
            let mut state = AppState::default();
            state.expanded_view = ExpandedView::Tasks;
            crate::state::store::AppStore::new(state, None)
        };
        let text = render_panel(short_store.clone(), false, 12);
        assert_eq!(text.lines().count(), 1, "canvas=\n{text}");
        assert_eq!(short_store.get().expanded_view, ExpandedView::Tasks);
    }

    #[test]
    fn connected_panel_rereads_only_when_the_revision_changes() {
        use crate::state::app_state_store::{AppState, ExpandedView};
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("panel-revision");
        let list_id = crate::utils::tasks::get_task_list_id();
        write_task(&list_id, "First", "in_progress");

        let mut state = AppState::default();
        state.expanded_view = ExpandedView::Tasks;
        let store = crate::state::store::AppStore::new(state, None);
        let text = render_panel(store.clone(), false, 30);
        assert!(
            text.contains("First") && !text.contains("Second"),
            "canvas=\n{text}"
        );

        // A fresh mount always loads, so assert the cache rule on the pure part:
        // the revision a mounted panel keyed on is what gates the reload.
        write_task(&list_id, "Second", "pending");
        store.replace_with(|state| state.task_list_revision += 1);
        let text = render_panel(store, false, 30);
        assert!(text.contains("Second"), "canvas=\n{text}");
    }

    #[test]
    fn dismissing_a_finished_list_hides_the_all_done_line_until_something_changes() {
        use crate::state::app_state_store::{AppState, ExpandedView};
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("panel-dismiss");
        let list_id = crate::utils::tasks::get_task_list_id();
        write_task(&list_id, "A", "completed");
        write_task(&list_id, "B", "completed");

        let mut state = AppState::default();
        state.expanded_view = ExpandedView::Tasks;
        let store = crate::state::store::AppStore::new(state, None);
        assert!(render_panel(store.clone(), false, 30).contains("✔ 2/2 done"));

        dismiss_finished_tasks(&store);
        assert!(store.get().todo_cleared_signature.is_some());
        assert_eq!(render_panel(store.clone(), false, 30).trim(), "");

        // A new task changes the signature: the block comes back.
        write_task(&list_id, "C", "pending");
        store.replace_with(|state| state.task_list_revision += 1);
        let text = render_panel(store.clone(), false, 30);
        assert!(text.contains("◻ C"), "canvas=\n{text}");

        // An unfinished list is never recorded as cleared.
        let before = store.get().todo_cleared_signature;
        dismiss_finished_tasks(&store);
        assert_eq!(store.get().todo_cleared_signature, before);
    }

    #[test]
    fn no_line_exceeds_the_terminal_width_at_80_and_40() {
        let long = "a very long task title that keeps going and going well past any terminal";
        let tasks = vec![
            titled("1", TaskStatusV2::Completed, long),
            TaskV2 {
                owner: Some("somebody-with-a-long-name".into()),
                blocked_by: vec!["9".into()],
                ..titled("2", TaskStatusV2::InProgress, long)
            },
            TaskV2 {
                blocked_by: vec!["2".into()],
                ..titled("3", TaskStatusV2::Pending, long)
            },
            titled(
                "4",
                TaskStatusV2::Pending,
                "中文标题会占两列所以也要按显示宽度截断才行啊",
            ),
        ];
        let theme = *theme::current();
        for columns in [80usize, 40] {
            for (expanded, spinner) in [(true, true), (true, false), (false, true), (false, false)]
            {
                let lines = layout_panel(&data(tasks.clone(), expanded, spinner, columns), &theme);
                assert!(!lines.is_empty());
                for line in &lines {
                    assert!(
                        width(&line.text()) <= columns,
                        "columns={columns} expanded={expanded} spinner={spinner}: {:?}",
                        line.text()
                    );
                }
                let canvas = render(data(tasks.clone(), expanded, spinner, columns), columns);
                for line in canvas.to_string().lines() {
                    assert!(
                        width(line) <= columns,
                        "canvas line over {columns}: {line:?}"
                    );
                }
            }
        }
    }
}
