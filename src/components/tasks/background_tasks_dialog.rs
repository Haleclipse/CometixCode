//! Maps to: CC `components/tasks/BackgroundTasksDialog.tsx:1-1012`.
//!
//! The live dialog owns local task dispatch; typed actions remain available for
//! view fixtures and unported task types. Sorting, grouping, selection and the
//! single-item detail lifecycle stay in this original component owner.

use super::task_status_utils::TaskStatus;
use super::{
    AsyncAgentDetailData, AsyncAgentDetailDialog, BackgroundTask, BackgroundTaskData,
    DreamDetailData, DreamDetailDialog, InProcessTeammateDetailDialog, RemoteSessionDetailAction,
    RemoteSessionDetailData, RemoteSessionDetailDialog, ShellDetailData, ShellDetailDialog,
    TeammateDetailData,
};
use crate::components::design_system::dialog::Dialog;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Rust status representation adapter for CC `BackgroundTasksDialog.tsx#toListItem`.
fn task_status_from_runtime(value: &str) -> TaskStatus {
    match value {
        "pending" => TaskStatus::Pending,
        "completed" | "succeeded" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "killed" => TaskStatus::Killed,
        _ => TaskStatus::Running,
    }
}

/// Maps to CC `BackgroundTasksDialog.tsx:225-279,872-938`: native task projection.
/// Local agents retain the established registry-data / AppStore-notification adapter.
/// Remote, teammate, workflow, monitor-MCP and dream task projections remain unported.
pub(crate) fn background_task_items(
    tasks: &std::collections::BTreeMap<
        String,
        std::sync::Arc<crate::state::app_state_store::TaskState>,
    >,
) -> Vec<BackgroundTasksDialogItem> {
    // CC BackgroundTaskStatus.tsx:49-57 and BackgroundTasksDialog.tsx:225-228
    // read the same AppState tasks and exclude foreground/terminal shells.
    let mut items: Vec<BackgroundTasksDialogItem> = tasks
        .values()
        .filter(|task| crate::tasks::types::is_background_task(task))
        .filter_map(|task| crate::tasks::local_shell_task::guards::is_local_shell_task(task))
        .map(|task| {
            let status = task_status_from_runtime(&task.status);
            BackgroundTasksDialogItem {
                id: task.id.clone(),
                category: BackgroundTaskCategory::Shell,
                label: if task.kind.as_deref() == Some("monitor") {
                    task.description.clone()
                } else {
                    task.command.clone()
                },
                status,
                start_time: task.start_time_ms.max(0) as u64,
                row: Some(BackgroundTaskData::LocalBash {
                    command: task.command.clone(),
                    monitor_description: (task.kind.as_deref() == Some("monitor"))
                        .then(|| task.description.clone()),
                    status,
                }),
                detail: BackgroundTaskDetailData::Shell(ShellDetailData {
                    id: task.id.clone(),
                    is_monitor: task.kind.as_deref() == Some("monitor"),
                    status,
                    exit_code: task.result.as_ref().map(|result| result.code),
                    runtime: crate::utils::format::format_duration(
                        crate::utils::task::framework::now_ms()
                            .saturating_sub(task.start_time_ms.max(0) as u64),
                    ),
                    command: task.command.clone(),
                    output: None,
                }),
                team_name: None,
            }
        })
        .collect();
    // Maps to: CC `tasks/types.ts:37-46` `isBackgroundTask` over the
    // local_agent entries (running|pending only; foreground agents with
    // `isBackgrounded === false` excluded) + CC
    // `BackgroundTasksDialog.tsx:885-892` `toListItem` for the local_agent
    // shape. The Rust items source is the registry snapshot, which carries the
    // same fields the AppState mirror projects.
    let mut agent_tasks: Vec<_> = crate::tasks::local_agent_task::local_agent_tasks_snapshot()
        .into_iter()
        .filter(|task| {
            matches!(task.status.as_str(), "running" | "pending") && task.is_backgrounded
        })
        .collect();
    // Maps to: CC `BackgroundTasksDialog.tsx:229-236` — running first, then
    // startTime desc. The registry snapshot comes from HashMap::values()
    // (nondeterministic), so the trailing id tiebreak is determinism-only:
    // CC's sort is stable over object insertion order, which a HashMap
    // cannot supply.
    agent_tasks.sort_by(|a, b| {
        let a_running = a.status == "running";
        let b_running = b.status == "running";
        b_running
            .cmp(&a_running)
            .then(b.start_time_ms.cmp(&a.start_time_ms))
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    for task in agent_tasks {
        let status = task_status_from_runtime(&task.status);
        // CC `AsyncAgentDetailDialog.tsx:79-82` — result stats win over
        // progress (result is always None on this running-only path).
        let token_count = task
            .result
            .as_ref()
            .map(|result| result.total_tokens)
            .or_else(|| task.progress.as_ref().map(|progress| progress.token_count));
        let tool_use_count = task
            .result
            .as_ref()
            .map(|result| result.total_tool_use_count)
            .or_else(|| {
                task.progress
                    .as_ref()
                    .map(|progress| progress.tool_use_count)
            });
        // Pre-computed `Tool.getActivityDescription` strings recorded at
        // progress time. CC's dialog re-renders `userFacingName(args)` live
        // through `renderToolActivity` (AsyncAgentDetailDialog.tsx:164); the
        // recorded description is the stable Rust stand-in, with the tool name
        // as CC's own fallback.
        let recent_activities = task
            .progress
            .as_ref()
            .map(|progress| {
                progress
                    .recent_activities
                    .iter()
                    .map(|activity| {
                        activity
                            .activity_description
                            .clone()
                            .unwrap_or_else(|| activity.tool_name.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // CC `AsyncAgentDetailDialog.tsx:37-42` `useElapsedTime(startTime, …)`
        // (totalPausedMs is not tracked on the Rust task state).
        let elapsed = crate::utils::format::format_duration(
            crate::utils::task::framework::now_ms().saturating_sub(task.start_time_ms),
        );
        items.push(BackgroundTasksDialogItem {
            id: task.task_id.clone(),
            category: BackgroundTaskCategory::LocalAgent,
            label: task.description.clone(),
            status,
            start_time: task.start_time_ms,
            row: Some(BackgroundTaskData::LocalAgent {
                description: task.description.clone(),
                status,
                notified: task.notified,
            }),
            detail: BackgroundTaskDetailData::Agent(AsyncAgentDetailData {
                agent_type: Some(task.agent_type.clone()),
                description: task.description,
                prompt: task.prompt,
                status,
                elapsed,
                token_count,
                tool_use_count,
                recent_activities,
                error: task.error,
            }),
            team_name: None,
        });
    }
    items
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackgroundTaskCategory {
    Teammate,
    Shell,
    Monitor,
    Remote,
    LocalAgent,
    Workflow,
    Dream,
    Leader,
}
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundTaskDetailData {
    Shell(ShellDetailData),
    Agent(AsyncAgentDetailData),
    Remote(RemoteSessionDetailData),
    Teammate(TeammateDetailData),
    Dream(DreamDetailData),
    Unsupported,
}
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTasksDialogItem {
    pub id: String,
    pub category: BackgroundTaskCategory,
    pub label: String,
    pub status: TaskStatus,
    pub start_time: u64,
    pub row: Option<BackgroundTaskData>,
    pub detail: BackgroundTaskDetailData,
    pub team_name: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundTasksDialogAction {
    Stop(String),
    Foreground(String),
    ViewLeader,
    Remote(RemoteSessionDetailAction),
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum ViewState {
    List,
    Detail(String),
}
#[derive(Clone, Copy)]
enum InputAction {
    Previous,
    Next,
    Accept,
    Back,
    Stop,
    Foreground,
}

fn selectable_items(
    items: &[BackgroundTasksDialogItem],
    foregrounded: Option<&str>,
    spinner_tree: bool,
) -> Vec<BackgroundTasksDialogItem> {
    let mut items = items
        .iter()
        .filter(|item| {
            !(item.category == BackgroundTaskCategory::LocalAgent
                && foregrounded == Some(item.id.as_str()))
                && !(spinner_tree && item.category == BackgroundTaskCategory::Teammate)
        })
        .cloned()
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        let a_running = a.status == TaskStatus::Running;
        let b_running = b.status == TaskStatus::Running;
        b_running
            .cmp(&a_running)
            .then_with(|| b.start_time.cmp(&a.start_time))
    });
    let order = [
        BackgroundTaskCategory::Teammate,
        BackgroundTaskCategory::Leader,
        BackgroundTaskCategory::Shell,
        BackgroundTaskCategory::Monitor,
        BackgroundTaskCategory::Remote,
        BackgroundTaskCategory::LocalAgent,
        BackgroundTaskCategory::Workflow,
        BackgroundTaskCategory::Dream,
    ];
    order
        .into_iter()
        .flat_map(|category| {
            items
                .iter()
                .filter(move |item| item.category == category)
                .cloned()
        })
        .collect()
}

#[derive(Default, Props)]
pub struct BackgroundTasksDialogProps<'a> {
    /// Maps to CC `Props.toolUseContext`; Some selects the live task store.
    pub tool_use_context: Option<Arc<crate::tool::ToolUseContext>>,
    /// Native footer carrier for the same AppStore, without synthesizing a ToolUseContext.
    pub app_store: Option<crate::state::store::AppStore>,
    /// Pure view fixtures; live callers supply tool_use_context or app_store.
    pub items: Vec<BackgroundTasksDialogItem>,
    pub foregrounded_task_id: Option<String>,
    pub show_spinner_tree: bool,
    pub initial_detail_task_id: Option<String>,
    pub on_done: HandlerMut<'a, String>,
    pub on_action: HandlerMut<'a, BackgroundTasksDialogAction>,
}

/// Maps to CC `BackgroundTasksDialog.tsx:392-410` local kill dispatch.
/// Both live entries use the same runtime actor; fixture callbacks stay available.
fn dispatch_action(
    props: &mut BackgroundTasksDialogProps<'_>,
    action: BackgroundTasksDialogAction,
    store: Option<&crate::state::store::AppStore>,
) {
    if props.tool_use_context.is_some() || props.app_store.is_some() {
        if let BackgroundTasksDialogAction::Stop(id) = &action {
            if crate::tasks::local_agent_task::task_identity_snapshot(id).is_some() {
                crate::tasks::local_agent_task::kill_async_agent(id);
            } else {
                crate::tasks::local_shell_task::kill_shell_tasks::kill_task(id, store);
            }
            return;
        }
        // Other task types are not produced by this batch's live projection.
        // Preserve fixture/custom caller callbacks; no runtime support is claimed.
    }
    (props.on_action)(action);
}

#[component]
pub fn BackgroundTasksDialog<'a>(
    props: &mut BackgroundTasksDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    // Source useAppState selectors subscribe to the mirror; local-agent data
    // retains the existing registry adapter. No filesystem I/O in this snapshot.
    let live_store = props
        .tool_use_context
        .as_ref()
        .and_then(|context| context.app_store.store.clone())
        .or_else(|| props.app_store.clone());
    let live = props.tool_use_context.is_some() || props.app_store.is_some();
    let subscribed =
        crate::state::app_state::use_app_state_maybe_outside_of_provider(&mut hooks, |state| {
            (
                state.tasks.clone(),
                state.foregrounded_task_id.clone(),
                state.expanded_view,
            )
        });
    let live_state = if live {
        subscribed.or_else(|| {
            live_store.as_ref().map(|store| {
                let state = store.get();
                (
                    state.tasks.clone(),
                    state.foregrounded_task_id.clone(),
                    state.expanded_view,
                )
            })
        })
    } else {
        None
    };
    let (source_items, foregrounded, spinner_tree) =
        if let Some((tasks, foregrounded, view)) = live_state {
            (
                background_task_items(&tasks),
                foregrounded,
                view == crate::state::app_state_store::ExpandedView::Teammates,
            )
        } else {
            (
                props.items.clone(),
                props.foregrounded_task_id.clone(),
                props.show_spinner_tree,
            )
        };
    let items = selectable_items(&source_items, foregrounded.as_deref(), spinner_tree);
    let initial_id = props
        .initial_detail_task_id
        .clone()
        .or_else(|| (items.len() == 1).then(|| items[0].id.clone()));
    // CC skippedListOnMount is a ref, not a fresh decision after task changes.
    let mut skipped_list_on_mount = hooks.use_state(|| initial_id.is_some());
    let overlay_store = hooks
        .try_use_context::<crate::state::store::AppStore>()
        .map(|store| store.clone());
    // CC BackgroundTasksDialog.tsx:211. The guard covers list, details and
    // removal transitions, and unregisters only when the whole dialog unmounts.
    let _overlay = hooks.use_state(move || {
        overlay_store.map(|store| {
            crate::context::overlay_context::OverlayRegistration::register(
                store,
                "background-tasks-dialog",
            )
        })
    });
    let mut view =
        hooks.use_state(move || initial_id.map(ViewState::Detail).unwrap_or(ViewState::List));
    let mut selected = hooks.use_state(|| 0usize);
    let mut pending = hooks.use_state(|| None::<InputAction>);
    let mut pending_action = hooks.use_state(|| None::<BackgroundTasksDialogAction>);
    let mut pending_done = hooks.use_state(|| None::<String>);
    let owner_action = { pending_action.read().clone() };
    if let Some(action) = owner_action {
        pending_action.set(None);
        dispatch_action(props, action, live_store.as_ref());
    }
    let done = { pending_done.read().clone() };
    if let Some(message) = done {
        pending_done.set(None);
        (props.on_done)(message);
    }
    if selected.get() >= items.len() && !items.is_empty() {
        selected.set(items.len() - 1);
    }
    let action = { *pending.read() };
    if let Some(action) = action {
        pending.set(None);
        let current = items.get(selected.get());
        match action {
            InputAction::Previous => selected.set(selected.get().saturating_sub(1)),
            InputAction::Next => {
                selected.set((selected.get() + 1).min(items.len().saturating_sub(1)))
            }
            InputAction::Accept => {
                if let Some(item) = current {
                    if item.category == BackgroundTaskCategory::Leader {
                        dispatch_action(
                            props,
                            BackgroundTasksDialogAction::ViewLeader,
                            live_store.as_ref(),
                        );
                        (props.on_done)("Viewing leader".to_string());
                    } else {
                        view.set(ViewState::Detail(item.id.clone()));
                    }
                }
            }
            InputAction::Back => {
                let back_view = { view.read().clone() };
                match back_view {
                    ViewState::List => {
                        (props.on_done)("Background tasks dialog dismissed".to_string())
                    }
                    ViewState::Detail(_) if skipped_list_on_mount.get() && items.len() <= 1 => {
                        (props.on_done)("Background tasks dialog dismissed".to_string())
                    }
                    ViewState::Detail(_) => {
                        skipped_list_on_mount.set(false);
                        view.set(ViewState::List);
                    }
                }
            }
            InputAction::Stop => {
                if let Some(item) = current.filter(|item| {
                    item.status == TaskStatus::Running
                        && item.category != BackgroundTaskCategory::Leader
                }) {
                    dispatch_action(
                        props,
                        BackgroundTasksDialogAction::Stop(item.id.clone()),
                        live_store.as_ref(),
                    );
                }
            }
            InputAction::Foreground => {
                if let Some(item) = current {
                    if item.category == BackgroundTaskCategory::Teammate {
                        dispatch_action(
                            props,
                            BackgroundTasksDialogAction::Foreground(item.id.clone()),
                            live_store.as_ref(),
                        );
                        (props.on_done)("Viewing teammate".to_string());
                    } else if item.category == BackgroundTaskCategory::Leader {
                        dispatch_action(
                            props,
                            BackgroundTasksDialogAction::ViewLeader,
                            live_store.as_ref(),
                        );
                        (props.on_done)("Viewing leader".to_string());
                    }
                }
            }
        }
    }
    let current_view = { view.read().clone() };
    let list_active = current_view == ViewState::List;
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    for (name, context, action, active) in [
        (
            "confirm:previous",
            ContextName::Confirmation,
            InputAction::Previous,
            list_active,
        ),
        (
            "confirm:next",
            ContextName::Confirmation,
            InputAction::Next,
            list_active,
        ),
        (
            "confirm:yes",
            ContextName::Confirmation,
            InputAction::Accept,
            list_active,
        ),
        (
            "confirm:no",
            ContextName::Confirmation,
            InputAction::Back,
            true,
        ),
        ("task:back", ContextName::Task, InputAction::Back, true),
        (
            "task:stop",
            ContextName::Task,
            InputAction::Stop,
            list_active,
        ),
        (
            "task:foreground",
            ContextName::Task,
            InputAction::Foreground,
            list_active,
        ),
    ] {
        let mut pending = pending;
        use_keybinding(
            &mut hooks,
            runtime.clone(),
            name,
            context,
            move || active,
            move || {
                pending.set(Some(action));
                true
            },
        );
    }

    if let ViewState::Detail(id) = current_view {
        let Some(item) = items.iter().find(|item| item.id == id) else {
            // CC :419-445: vanished/finished details return to the original
            // list unless the caller entered directly into a single detail.
            if skipped_list_on_mount.get() {
                pending_done.set(Some("Background tasks dialog dismissed".to_string()));
            } else {
                view.set(ViewState::List);
            }
            return element! { Fragment }.into_any();
        };
        let mut done = pending_done;
        let mut back = pending;
        let mut stop = pending_action;
        let item_id = item.id.clone();
        return match &item.detail {
            BackgroundTaskDetailData::Shell(data) => element! { ShellDetailDialog(key: format!("shell-{}", item.id), shell: Some(data.clone()), on_done: move |message| done.set(Some(message)), on_back: move |_| back.set(Some(InputAction::Back)), on_kill_shell: move |_| stop.set(Some(BackgroundTasksDialogAction::Stop(item_id.clone())))) }.into_any(),
            BackgroundTaskDetailData::Agent(data) => element! { AsyncAgentDetailDialog(key: format!("agent-{}", item.id), agent: Some(data.clone()), on_done: move |_| done.set(Some("Background tasks dialog dismissed".to_string())), on_back: move |_| back.set(Some(InputAction::Back)), on_kill_agent: move |_| stop.set(Some(BackgroundTasksDialogAction::Stop(item_id.clone())))) }.into_any(),
            BackgroundTaskDetailData::Remote(data) => element! { RemoteSessionDetailDialog(session: Some(data.clone()), on_done: move |message| done.set(Some(message)), on_back: move |_| back.set(Some(InputAction::Back)), on_kill: move |_| stop.set(Some(BackgroundTasksDialogAction::Stop(item_id.clone()))), on_action: move |action| stop.set(Some(BackgroundTasksDialogAction::Remote(action)))) }.into_any(),
            BackgroundTaskDetailData::Teammate(data) => {
                let kill_id = item_id.clone();
                let foreground_id = item_id.clone();
                element! { InProcessTeammateDetailDialog(teammate: Some(data.clone()), on_done: move |_| done.set(Some("Background tasks dialog dismissed".to_string())), on_back: move |_| back.set(Some(InputAction::Back)), on_kill: move |_| stop.set(Some(BackgroundTasksDialogAction::Stop(kill_id.clone()))), on_foreground: move |_| stop.set(Some(BackgroundTasksDialogAction::Foreground(foreground_id.clone())))) }.into_any()
            },
            BackgroundTaskDetailData::Dream(data) => element! { DreamDetailDialog(task: Some(data.clone()), on_done: move |_| done.set(Some("Background tasks dialog dismissed".to_string())), on_back: move |_| back.set(Some(InputAction::Back)), on_kill: move |_| stop.set(Some(BackgroundTasksDialogAction::Stop(item_id.clone())))) }.into_any(),
            BackgroundTaskDetailData::Unsupported => { view.set(ViewState::List); element! { Fragment }.into_any() }
        };
    }

    let running_shells = items
        .iter()
        .filter(|item| {
            item.category == BackgroundTaskCategory::Shell && item.status == TaskStatus::Running
        })
        .count();
    let running_agents = items
        .iter()
        .filter(|item| {
            matches!(
                item.category,
                BackgroundTaskCategory::Remote | BackgroundTaskCategory::LocalAgent
            ) && matches!(item.status, TaskStatus::Running | TaskStatus::Pending)
        })
        .count();
    let running_teammates = items
        .iter()
        .filter(|item| {
            item.category == BackgroundTaskCategory::Teammate && item.status == TaskStatus::Running
        })
        .count();
    let subtitle = [
        (running_teammates, "agent", "agents"),
        (running_shells, "active shell", "active shells"),
        (running_agents, "active agent", "active agents"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
    .collect::<Vec<_>>()
    .join(" · ");
    let current = items.get(selected.get());
    let guide = format!(
        "↑/↓ to select · Enter to view{}{} · ←/Esc to close",
        if current.is_some_and(|item| item.category == BackgroundTaskCategory::Teammate
            && item.status == TaskStatus::Running)
        {
            " · f to foreground"
        } else {
            ""
        },
        if current.is_some_and(|item| item.category != BackgroundTaskCategory::Leader
            && item.status == TaskStatus::Running)
        {
            " · x to stop"
        } else {
            ""
        }
    );
    let mut groups = BTreeMap::<BackgroundTaskCategory, Vec<&BackgroundTasksDialogItem>>::new();
    for item in &items {
        groups.entry(item.category).or_default().push(item);
    }
    let order = [
        BackgroundTaskCategory::Teammate,
        BackgroundTaskCategory::Leader,
        BackgroundTaskCategory::Shell,
        BackgroundTaskCategory::Monitor,
        BackgroundTaskCategory::Remote,
        BackgroundTaskCategory::LocalAgent,
        BackgroundTaskCategory::Workflow,
        BackgroundTaskCategory::Dream,
    ];
    let mut rendered = Vec::<AnyElement<'static>>::new();
    for category in order {
        let Some(group) = groups.get(&category) else {
            continue;
        };
        if category != BackgroundTaskCategory::Leader
            && !(category == BackgroundTaskCategory::Dream && group.len() == 1)
        {
            let label = match category {
                BackgroundTaskCategory::Teammate => "Agents",
                BackgroundTaskCategory::Shell => "Shells",
                BackgroundTaskCategory::Monitor => "Monitors",
                BackgroundTaskCategory::Remote => "Remote agents",
                BackgroundTaskCategory::LocalAgent => "Local agents",
                BackgroundTaskCategory::Workflow => "Workflows",
                BackgroundTaskCategory::Dream => "Dreams",
                BackgroundTaskCategory::Leader => "",
            };
            rendered.push(element! { Text(content: format!("  {label} ({})", group.len()), weight: Weight::Bold, dim: true) }.into_any());
        }
        for item in group {
            let selected_row = current.is_some_and(|current| current.id == item.id);
            let row: AnyElement<'static> = if category == BackgroundTaskCategory::Leader {
                element! { Text(content: "@team-lead".to_string()) }.into_any()
            } else {
                item.row.clone().map(|row| element! { BackgroundTask(task: Some(row), max_activity_width: Some(60usize)) }.into_any()).unwrap_or_else(|| element! { Text(content: item.label.clone()) }.into_any())
            };
            rendered.push(element! { View(flex_direction: FlexDirection::Row) { Text(content: if selected_row { "❯ ".to_string() } else { "  ".to_string() }, color: selected_row.then_some(hooks.use_context::<Theme>().suggestion)) #(vec![row]) } }.into_any());
        }
    }
    let theme = hooks.use_context::<Theme>();
    let mut cancel = pending;
    element! { Dialog(title: "Background tasks".to_string(), subtitle: (!subtitle.is_empty()).then_some(subtitle), color: Some(theme.background), input_guide: Some(guide), on_cancel: move |_| cancel.set(Some(InputAction::Back))) {
        #(if rendered.is_empty() { vec![element! { Text(content: "No tasks currently running".to_string(), dim: true) }.into_any()] } else { rendered })
    }}.into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(
        id: &str,
        category: BackgroundTaskCategory,
        status: TaskStatus,
        start: u64,
    ) -> BackgroundTasksDialogItem {
        BackgroundTasksDialogItem {
            id: id.to_string(),
            category,
            label: id.to_string(),
            status,
            start_time: start,
            row: None,
            detail: BackgroundTaskDetailData::Unsupported,
            team_name: None,
        }
    }
    #[test]
    fn selectable_items_matches_official_running_before_newer_pending() {
        // CC BackgroundTasksDialog.tsx:229-236: only "running" gets priority;
        // this is the final list used by keyboard navigation and rendering.
        let values = vec![
            item(
                "pending-newest",
                BackgroundTaskCategory::LocalAgent,
                TaskStatus::Pending,
                3000,
            ),
            item(
                "running-oldest",
                BackgroundTaskCategory::LocalAgent,
                TaskStatus::Running,
                1000,
            ),
            item(
                "running-newer",
                BackgroundTaskCategory::LocalAgent,
                TaskStatus::Running,
                2000,
            ),
        ];
        let final_list = selectable_items(&values, None, false);
        assert_eq!(
            final_list
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["running-newer", "running-oldest", "pending-newest"]
        );
    }

    #[test]
    fn filtering_sorting_and_category_order_match_list_contract() {
        let items = vec![
            item(
                "old-shell",
                BackgroundTaskCategory::Shell,
                TaskStatus::Completed,
                1,
            ),
            item(
                "agent",
                BackgroundTaskCategory::LocalAgent,
                TaskStatus::Running,
                2,
            ),
            item(
                "new-shell",
                BackgroundTaskCategory::Shell,
                TaskStatus::Running,
                3,
            ),
        ];
        let result = selectable_items(&items, Some("agent"), false);
        assert_eq!(
            result
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new-shell", "old-shell"]
        );
    }
    #[test]
    fn list_renders_categories_summary_selection_and_guidance() {
        let items = vec![
            item(
                "shell",
                BackgroundTaskCategory::Shell,
                TaskStatus::Running,
                1,
            ),
            item(
                "remote",
                BackgroundTaskCategory::Remote,
                TaskStatus::Running,
                2,
            ),
        ];
        let text = element! { ContextProvider(value: Context::owned(*crate::utils::theme::current())) { BackgroundTasksDialog(items: items) } }.render(Some(120)).to_string();
        assert!(text.contains("Background tasks"));
        assert!(text.contains("1 active shell · 1 active agent"));
        assert!(text.contains("Shells (1)"));
        assert!(text.contains("Remote agents (1)"));
        assert!(text.contains("x to stop"));
    }

    #[derive(Default, Props)]
    struct DialogProbeProps {
        items: Vec<BackgroundTasksDialogItem>,
        cancellations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[component]
    fn DialogProbe(props: &DialogProbeProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let store = hooks.use_context::<crate::state::store::AppStore>().clone();
        let mut open = hooks.use_state(|| true);
        let mut items = hooks.use_state(|| props.items.clone());
        let cancellations = props.cancellations.clone();
        crate::hooks::use_cancel_request::use_cancel_request(
            &mut hooks,
            crate::hooks::use_cancel_request::UseCancelRequestOptions {
                app_store: store,
                can_cancel_running_task: || true,
                on_cancel: move || {
                    cancellations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
                is_context_blocked: false,
            },
        );
        hooks.use_propagated_terminal_events(move |event| {
            if let TerminalEvent::Key(key) = event.event() {
                match key.code {
                    KeyCode::F(2) => {
                        let next = items
                            .read()
                            .iter()
                            .filter(|item| item.id != "a")
                            .cloned()
                            .collect();
                        items.set(next);
                    }
                    KeyCode::F(3) => {
                        let mut next = items.read().clone();
                        next.push(shell_item("b"));
                        items.set(next);
                    }
                    KeyCode::F(4) => {
                        let next = items
                            .read()
                            .iter()
                            .filter(|item| item.id != "b")
                            .cloned()
                            .collect();
                        items.set(next);
                    }
                    _ => return,
                }
                event.stop_propagation();
            }
        });
        element! { View(flex_direction: FlexDirection::Column) {
            Text(content: if open.get() { "probe-open" } else { "probe-closed" })
            #(open.get().then(|| element! { BackgroundTasksDialog(
                items: items.read().clone(), on_done: move |_| open.set(false),
            ) }))
        }}
    }

    fn shell_item(id: &str) -> BackgroundTasksDialogItem {
        let mut value = item(id, BackgroundTaskCategory::Shell, TaskStatus::Running, 1);
        value.detail = BackgroundTaskDetailData::Shell(ShellDetailData {
            id: id.into(),
            status: TaskStatus::Running,
            command: format!("shell-{id}"),
            output: Some(Default::default()),
            ..Default::default()
        });
        value
    }

    async fn wait_probe_frame<S>(frames: &mut S, needle: &str) -> String
    where
        S: futures::Stream<Item = Canvas> + Unpin,
    {
        use futures::StreamExt;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            let next = crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(250)).await;
                None
            })
            .await;
            if let Some(canvas) = next {
                last = canvas.to_string();
                if last.contains(needle) {
                    return last;
                }
            }
        }
        panic!("missing {needle}; final frame: {last}");
    }

    #[test]
    fn task_dialog_matches_official_escape_and_removal_lifecycle() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        futures::executor::block_on(async {
            // CC BackgroundTasksDialog.tsx:186-211,419-461 +
            // useCancelRequest.ts:141-154: modal ownership survives task changes.
            for initial_count in [1, 2] {
                let store = crate::state::store::AppStore::new(Default::default(), None);
                let cancellations = Arc::new(AtomicUsize::new(0));
                let runtime = KeybindingRuntime::with_default_bindings();
                let mut items = vec![shell_item("a")];
                if initial_count == 2 {
                    items.push(shell_item("b"));
                }
                let (tx, rx) = async_channel::unbounded();
                let mut app = element! { ContextProvider(value: Context::owned(store.clone())) {
                    ContextProvider(value: Context::owned(runtime)) {
                        ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                            DialogProbe(items: items, cancellations: cancellations.clone())
                        }
                    }
                }};
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(rx).with_size(100, 30),
                ));
                wait_probe_frame(
                    &mut frames,
                    if initial_count == 1 {
                        "Shell details"
                    } else {
                        "Background tasks"
                    },
                )
                .await;
                assert!(
                    store
                        .get()
                        .active_overlays
                        .contains("background-tasks-dialog")
                );
                let send = |code| {
                    tx.try_send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
                        .unwrap()
                };
                if initial_count == 2 {
                    send(KeyCode::Enter);
                    wait_probe_frame(&mut frames, "shell-a").await;
                    send(KeyCode::F(2));
                    wait_probe_frame(&mut frames, "Background tasks").await;
                    assert!(
                        store
                            .get()
                            .active_overlays
                            .contains("background-tasks-dialog")
                    );
                    send(KeyCode::Enter);
                    wait_probe_frame(&mut frames, "shell-b").await;
                    send(KeyCode::Left);
                    // Initially a list: shrinking to one item must not turn Back into close.
                    wait_probe_frame(&mut frames, "Background tasks").await;
                }
                send(KeyCode::Esc);
                wait_probe_frame(&mut frames, "probe-closed").await;
                assert_eq!(
                    cancellations.load(Ordering::SeqCst),
                    0,
                    "dismissal aborted main query"
                );
                assert!(!crate::context::overlay_context::is_overlay_active(&store));
                send(KeyCode::Esc);
                // Poll the real cancel hook after the dialog is unmounted.
                let _ = crate::utils::race(futures::StreamExt::next(&mut frames), async {
                    futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;
                    None
                })
                .await;
                assert_eq!(cancellations.load(Ordering::SeqCst), 1);
            }
        });
    }

    #[test]
    fn task_dialog_matches_official_initial_skip_reset_after_back() {
        futures::executor::block_on(async {
            let store = crate::state::store::AppStore::new(Default::default(), None);
            let (tx, rx) = async_channel::unbounded();
            let mut app = element! { ContextProvider(value: Context::owned(store)) {
                ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        DialogProbe(items: vec![shell_item("a")])
                    }
                }
            }};
            let mut frames =
                Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(rx).with_size(100, 30),
                ));
            let send = |code| {
                tx.try_send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
                    .unwrap()
            };
            wait_probe_frame(&mut frames, "Shell details").await;
            send(KeyCode::F(3)); // a second task arrives after initial direct detail
            // A second task arriving leaves the directly-opened detail view
            // unchanged, so there is no new frame to wait for here — waiting
            // used to pass only on the 80→100 divider re-layout frame, not on
            // F3's effect. The list reached via Back proves F3 landed.
            send(KeyCode::Left);
            let list = wait_probe_frame(&mut frames, "Background tasks").await;
            assert!(list.contains("Shells (2)"), "F3 task missing from list: {list}");
            send(KeyCode::F(4));
            wait_probe_frame(&mut frames, "Shells (1)").await;
            send(KeyCode::Enter);
            wait_probe_frame(&mut frames, "Shell details").await;
            send(KeyCode::F(2));
            // CC :419-445,449-461: once Back has shown the list, completing
            // the final task returns to that list instead of dismissing it.
            let last = wait_probe_frame(&mut frames, "Background tasks").await;
            assert!(last.contains("probe-open"));
        });
    }
    #[test]
    fn background_task_items_list_only_backgrounded_running_local_agents() {
        // CC tasks/types.ts:37-46 isBackgroundTask over the local_agent items
        // + BackgroundTasksDialog.tsx:885-892 toListItem shape.
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );

        let backgrounded = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: backgrounded.clone(),
                description: "review code".to_string(),
                prompt: "look at the diff".to_string(),
                selected_agent: agent.clone(),
                tool_use_id: None,
            },
        );
        let foreground = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_agent_foreground(
            crate::tasks::local_agent_task::RegisterAgentForegroundParams {
                agent_id: foreground.clone(),
                description: "foreground".to_string(),
                prompt: "fg".to_string(),
                selected_agent: agent.clone(),
                auto_background_ms: None,
                tool_use_id: None,
            },
        );
        let completed = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: completed.clone(),
                description: "done already".to_string(),
                prompt: "done".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        crate::tasks::local_agent_task::complete_agent_task(
            &crate::tools::agent_tool::agent_tool_utils::CompletedAgentRun {
                agent_id: completed.clone(),
                agent_type: "general-purpose".to_string(),
                content: vec!["done".to_string()],
                messages: Vec::new(),
                total_tool_use_count: 0,
                total_duration_ms: 1,
                total_tokens: 2,
                usage: None,
                content_replacement_state: None,
            },
        );

        let items = crate::components::tasks::background_tasks_dialog::background_task_items(
            &Default::default(),
        );
        let entry = items
            .iter()
            .find(|item| item.id == backgrounded)
            .expect("backgrounded running agent is listed");
        assert_eq!(entry.category, BackgroundTaskCategory::LocalAgent);
        assert_eq!(entry.label, "review code");
        assert!(matches!(
            entry.row,
            Some(BackgroundTaskData::LocalAgent {
                notified: false,
                ..
            })
        ));
        match &entry.detail {
            BackgroundTaskDetailData::Agent(detail) => {
                assert_eq!(detail.agent_type.as_deref(), Some("general-purpose"));
                assert_eq!(detail.description, "review code");
                assert_eq!(detail.prompt, "look at the diff");
                assert_eq!(detail.status, TaskStatus::Running);
            }
            other => panic!("expected an Agent detail, got {other:?}"),
        }
        assert!(
            items.iter().all(|item| item.id != foreground),
            "foreground agents (isBackgrounded === false) are excluded (CC types.ts:42-44)"
        );
        assert!(
            items.iter().all(|item| item.id != completed),
            "terminal agents are excluded (CC types.ts:38-40)"
        );

        // The new slash/footer owner dispatch must reach LocalAgentTask.kill,
        // not merely emit a UI event. The registry is the existing native actor.
        let mut props = BackgroundTasksDialogProps {
            tool_use_context: Some(Arc::new(crate::tool::ToolUseContext::default())),
            ..Default::default()
        };
        dispatch_action(
            &mut props,
            BackgroundTasksDialogAction::Stop(backgrounded.clone()),
            None,
        );
        assert!(
            crate::tasks::local_agent_task::local_agent_tasks_snapshot()
                .iter()
                .any(|task| task.task_id == backgrounded && task.status == "killed")
        );
        assert!(
            background_task_items(&Default::default())
                .iter()
                .all(|item| item.id != backgrounded)
        );

        for id in [&backgrounded, &foreground, &completed] {
            let _ = crate::utils::task::disk_output::cleanup_task_output(id);
        }
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[test]
    fn background_task_agent_items_sort_running_first_then_start_time_desc() {
        // CC BackgroundTasksDialog.tsx:229-236 — running first, startTime
        // desc; the trailing id-asc tiebreak is Rust-only determinism over
        // the registry HashMap's iteration order.
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        let fixtures = [
            ("agent-b-tie", 2_000u64, "running"),
            ("agent-a-tie", 2_000, "running"),
            ("agent-old", 1_000, "running"),
            ("agent-pending", 3_000, "pending"),
        ];
        for (id, start, status) in fixtures {
            crate::tasks::local_agent_task::register_async_agent(
                crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                    agent_id: id.to_string(),
                    description: id.to_string(),
                    prompt: id.to_string(),
                    selected_agent: agent.clone(),
                    tool_use_id: None,
                },
            );
            crate::tasks::local_agent_task::update_task_state_and_mirror(id, |task| {
                task.start_time_ms = start;
                task.status = status.to_string();
            });
        }
        let agent_ids: Vec<String> =
            crate::components::tasks::background_tasks_dialog::background_task_items(
                &Default::default(),
            )
            .into_iter()
            .filter(|item| item.category == BackgroundTaskCategory::LocalAgent)
            .map(|item| item.id)
            .collect();
        assert_eq!(
            agent_ids,
            vec![
                "agent-a-tie".to_string(),
                "agent-b-tie".to_string(),
                "agent-old".to_string(),
                "agent-pending".to_string(),
            ],
            "running first (pending last despite newest startTime), \
             startTime desc, id asc on full ties"
        );
        for (id, _, _) in fixtures {
            let _ = crate::utils::task::disk_output::cleanup_task_output(id);
        }
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }
    fn live_shell(id: &str) -> crate::state::app_state_store::TaskState {
        crate::state::app_state_store::TaskState::LocalShell(
            crate::tasks::local_shell_task::guards::LocalShellTaskState {
                id: id.into(),
                task_type: "local_bash".into(),
                status: "running".into(),
                description: "a local shell".into(),
                command: format!("command-{id}"),
                result: None,
                notified: false,
                shell_command: None,
                last_reported_total_lines: 0,
                is_backgrounded: true,
                agent_id: None,
                tool_use_id: None,
                kind: None,
                start_time_ms: 1,
                end_time_ms: None,
            },
        )
    }

    #[derive(Default, Props)]
    struct LiveDialogProbeProps {
        context: Option<Arc<crate::tool::ToolUseContext>>,
        store: Option<crate::state::store::AppStore>,
    }

    #[component]
    fn LiveDialogProbe(
        props: &LiveDialogProbeProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut open = hooks.use_state(|| true);
        element! { View(flex_direction: FlexDirection::Column) {
            Text(content: if open.get() { "live-open" } else { "live-closed" })
            #(open.get().then(|| element! { BackgroundTasksDialog(
                tool_use_context: props.context.clone(), app_store: props.store.clone(),
                on_done: move |_| open.set(false),
            ) }))
        }}
    }

    #[test]
    fn both_live_entries_list_and_stop_shells_through_the_same_store() {
        futures::executor::block_on(async {
            for slash in [false, true] {
                let store = crate::state::store::AppStore::new(Default::default(), None);
                crate::utils::task::framework::register_task(live_shell("live-a"), &store);
                crate::utils::task::framework::register_task(live_shell("live-b"), &store);
                let context = slash.then(|| {
                    Arc::new(crate::tool::ToolUseContext::default().with_app_store(store.clone()))
                });
                let footer_store = (!slash).then(|| store.clone());
                let (tx, rx) = async_channel::unbounded();
                let mut app = element! { ContextProvider(value: Context::owned(store.clone())) {
                    ContextProvider(value: Context::owned(KeybindingRuntime::with_default_bindings())) {
                        ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                            LiveDialogProbe(context: context, store: footer_store)
                        }
                    }
                }};
                let mut frames = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(rx).with_size(100, 30),
                ));
                wait_probe_frame(&mut frames, "2 active shells").await;
                tx.try_send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char('x'),
                )))
                .unwrap();
                wait_probe_frame(&mut frames, "1 active shell").await;
                let killed = store.get().tasks.values().filter(|task| {
                    matches!(task.as_ref(), crate::state::app_state_store::TaskState::LocalShell(shell) if shell.status == "killed")
                }).count();
                assert_eq!(killed, 1, "stop must reach the live task actor");
                tx.try_send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Enter,
                )))
                .unwrap();
                wait_probe_frame(&mut frames, "Shell details").await;
                tx.try_send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Char('x'),
                )))
                .unwrap();
                wait_probe_frame(&mut frames, "No tasks currently running").await;
                assert!(store.get().tasks.values().all(|task| {
                    matches!(task.as_ref(), crate::state::app_state_store::TaskState::LocalShell(shell) if shell.status == "killed")
                }));
                tx.try_send(TerminalEvent::Key(KeyEvent::new(
                    KeyEventKind::Press,
                    KeyCode::Esc,
                )))
                .unwrap();
                wait_probe_frame(&mut frames, "live-closed").await;
                assert!(!crate::context::overlay_context::is_overlay_active(&store));
            }
        });
    }
}
