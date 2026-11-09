//! Maps to: CC `state/teammateViewHelpers.ts`.
//!
//! The three transitions that own `AppState.viewingAgentTaskId` +
//! `viewSelectionMode`. CC threads a `setAppState` updater into each helper;
//! Rust passes the `AppStore` itself, matching the shape `utils/task/framework.rs`
//! already uses (`evict_terminal_task(task_id, app_store)`) — `setAppState` and
//! the store are the same write channel.
//!
//! @nyan: CC calls `logEvent('tengu_transcript_view_enter')` at `:45` and
//! `logEvent('tengu_transcript_view_exit')` at `:94`. nyan has no analytics, so
//! those calls are intentionally absent.

use crate::state::app_state_store::{TaskState, TaskStateOther, ViewSelectionMode};
use crate::state::store::{AppStore, UpdateDecision};
use crate::tasks::in_process_teammate_task::is_terminal_task_status;
use crate::utils::task::framework::{PANEL_GRACE_MS, now_ms};
use std::sync::Arc;

/// Maps to: CC `:14-22 isLocalAgent(task)`.
///
/// CC declares this inline rather than importing `isLocalAgentTask`, to break a
/// runtime edge that would cycle through `BackgroundTasksDialog`. Rust has no
/// such cycle, but the narrowing is kept as its own function so the three
/// helpers read the same as the source.
///
/// `retain` lives only on CC's `LocalAgentTaskState`, which in the Rust union is
/// the `Other` stub carrying `task_type == "local_agent"`.
fn as_local_agent(task: &TaskState) -> Option<&TaskStateOther> {
    match task {
        TaskState::Other(other) if other.task_type == "local_agent" => Some(other),
        TaskState::Other(_)
        | TaskState::InProcessTeammate(_)
        | TaskState::LocalShell(_)
        | TaskState::Dream(_) => None,
    }
}

/// Maps to: CC `:27-38 release(task)` — the task back in stub form: retain
/// dropped, `evictAfter` set when terminal so the coordinator-panel row lingers
/// for the grace window. Shared by `exit_teammate_view` and the switch-away path
/// in `enter_teammate_view`.
///
/// SEAM: CC also clears `messages: undefined` and `diskLoaded: false`. Neither
/// field exists on the Rust task union yet — `TaskState`'s own doc records that
/// in-process entries carry only the spinner/UI snapshot subset "until the full
/// `InProcessTeammateTaskState` (with abort controllers / messages) lives here".
/// Nothing is lost today because there is nothing to clear; when those fields
/// land they MUST be cleared here.
fn release(task: &TaskStateOther) -> TaskStateOther {
    let mut released = task.clone();
    released.retain = Some(false);
    released.evict_after = is_terminal_task_status(&task.status).then(|| now_ms() + PANEL_GRACE_MS);
    released
}

/// Maps to: CC `:46-84 enterTeammateView`.
///
/// Sets `viewingAgentTaskId` and, for a local_agent, `retain: true` (blocks
/// eviction, enables stream-append, triggers disk bootstrap) while clearing
/// `evictAfter`. Switching away from another retained agent releases it first.
pub fn enter_teammate_view(task_id: &str, app_store: &AppStore) {
    // SEAM: CC `:45` logEvent('tengu_transcript_view_enter', {}).
    app_store.set_state(|prev| {
        let prev_id = prev.viewing_agent_task_id.as_deref();

        // CC `:52-56` — switching away from a *different*, retained local_agent.
        let switching = match prev_id {
            Some(pid) if pid != task_id => prev
                .tasks
                .get(pid)
                .and_then(|task| as_local_agent(task))
                .is_some_and(|task| task.retain == Some(true)),
            _ => false,
        };

        // CC `:57-58` — `isLocalAgent(task) && (!task.retain || task.evictAfter !== undefined)`.
        // `retain != Some(true)` is CC's `!task.retain`: false and absent both qualify.
        let needs_retain = prev
            .tasks
            .get(task_id)
            .and_then(|task| as_local_agent(task))
            .is_some_and(|task| task.retain != Some(true) || task.evict_after.is_some());

        // CC `:59-61`.
        let needs_view =
            prev_id != Some(task_id) || prev.view_selection_mode != ViewSelectionMode::ViewingAgent;

        // CC `:62` — `if (!needsRetain && !needsView && !switching) return prev`.
        if !needs_retain && !needs_view && !switching {
            return UpdateDecision::Same(());
        }

        let mut next = (**prev).clone();
        // CC `:63-71` — the task map is only copied when something in it changes.
        if switching || needs_retain {
            let tasks = Arc::make_mut(&mut next.tasks);
            if switching {
                let pid = prev_id.expect("switching implies a previous id");
                let released = tasks
                    .get(pid)
                    .and_then(|task| as_local_agent(task))
                    .map(release);
                if let Some(released) = released {
                    tasks.insert(pid.to_string(), Arc::new(TaskState::Other(released)));
                }
            }
            if needs_retain {
                // CC `:69` — `{ ...task, retain: true, evictAfter: undefined }`.
                let retained = tasks
                    .get(task_id)
                    .and_then(|task| as_local_agent(task))
                    .map(|task| {
                        let mut retained = task.clone();
                        retained.retain = Some(true);
                        retained.evict_after = None;
                        retained
                    });
                if let Some(retained) = retained {
                    tasks.insert(task_id.to_string(), Arc::new(TaskState::Other(retained)));
                }
            }
        }
        // CC `:72-77`.
        next.viewing_agent_task_id = Some(task_id.to_string());
        next.view_selection_mode = ViewSelectionMode::ViewingAgent;
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `:88-110 exitTeammateView` — back to the leader's view, dropping
/// retain so a terminal row lingers only for the grace window.
pub fn exit_teammate_view(app_store: &AppStore) {
    // SEAM: CC `:94` logEvent('tengu_transcript_view_exit', {}).
    app_store.set_state(|prev| {
        // CC `:96-101` — nothing viewed: reset the mode, and only when it is
        // not already 'none' (otherwise `return prev`).
        let Some(id) = prev.viewing_agent_task_id.clone() else {
            if prev.view_selection_mode == ViewSelectionMode::None {
                return UpdateDecision::Same(());
            }
            let mut next = (**prev).clone();
            next.view_selection_mode = ViewSelectionMode::None;
            return UpdateDecision::Replace {
                next: Arc::new(next),
                result: (),
            };
        };

        let mut next = (**prev).clone();
        next.viewing_agent_task_id = None;
        next.view_selection_mode = ViewSelectionMode::None;

        // CC `:103-108` — a non-local_agent or non-retained task is left alone;
        // only `cleared` is returned.
        let released = prev
            .tasks
            .get(&id)
            .and_then(|task| as_local_agent(task))
            .filter(|task| task.retain == Some(true))
            .map(release);
        if let Some(released) = released {
            Arc::make_mut(&mut next.tasks).insert(id, Arc::new(TaskState::Other(released)));
        }
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `:116-140 stopOrDismissAgent` — context-sensitive `x`:
/// running → abort, terminal → dismiss (`evictAfter = 0`, so the panel filter
/// hides it immediately). Dismissing the agent currently being viewed also
/// exits to the leader.
///
/// **Returns `true` when the caller must abort a still-running task.**
///
/// Deviation, recorded: CC calls `task.abortController?.abort()` *inside* the
/// `setAppState` updater and then returns `prev` (`:121-124`) — a side effect
/// smuggled into a pure updater. Rust cannot reproduce that: the abort handle is
/// not on the task union (see `release`'s SEAM note), and the Rust stop channel
/// `tasks::stop_task::stop_task` is `async`, which a synchronous `set_state`
/// updater cannot await. So the decision is returned to the caller instead,
/// mirroring how `McpWriter::initialize_servers_as_pending` hands stale clients
/// back for CC's fire-and-forget `clearServerCache` loop. State is left
/// untouched on this branch, exactly as CC leaves it.
#[must_use = "a `true` result means the caller must stop the still-running task"]
pub fn stop_or_dismiss_agent(task_id: &str, app_store: &AppStore) -> bool {
    app_store.set_state(|prev| {
        // CC `:118-119` — `if (!isLocalAgent(task)) return prev`.
        let Some(task) = prev
            .tasks
            .get(task_id)
            .and_then(|task| as_local_agent(task))
        else {
            return UpdateDecision::Same(false);
        };
        // CC `:120-123` — running: abort, state unchanged.
        if task.status == "running" {
            return UpdateDecision::Same(true);
        }
        // CC `:124` — already dismissed.
        if task.evict_after == Some(0) {
            return UpdateDecision::Same(false);
        }
        // CC `:125` + `:135-138`.
        let viewing_this = prev.viewing_agent_task_id.as_deref() == Some(task_id);
        let mut dismissed = release(task);
        dismissed.evict_after = Some(0);

        let mut next = (**prev).clone();
        Arc::make_mut(&mut next.tasks)
            .insert(task_id.to_string(), Arc::new(TaskState::Other(dismissed)));
        if viewing_this {
            next.viewing_agent_task_id = None;
            next.view_selection_mode = ViewSelectionMode::None;
        }
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;

    fn local_agent(id: &str, status: &str) -> Arc<TaskState> {
        Arc::new(TaskState::Other(TaskStateOther {
            id: id.to_string(),
            task_type: "local_agent".to_string(),
            status: status.to_string(),
            description: String::new(),
            is_backgrounded: None,
            notified: false,
            retain: Some(false),
            evict_after: None,
            progress_tool_uses: None,
            progress_tokens: None,
        }))
    }

    fn store_with(tasks: Vec<(&str, Arc<TaskState>)>) -> AppStore {
        let mut state = AppState::default();
        let map = Arc::make_mut(&mut state.tasks);
        for (id, task) in tasks {
            map.insert(id.to_string(), task);
        }
        AppStore::new(state, None)
    }

    fn retain_of(store: &AppStore, id: &str) -> Option<bool> {
        match store.get().tasks.get(id)?.as_ref() {
            TaskState::Other(task) => task.retain,
            _ => None,
        }
    }

    /// Maps to: CC `:57-77` — entering sets both view fields and flips the
    /// target to `retain: true` with `evictAfter` cleared.
    #[test]
    fn entering_retains_the_target_and_sets_both_view_fields() {
        let store = store_with(vec![("a", local_agent("a", "running"))]);

        enter_teammate_view("a", &store);

        let state = store.get();
        assert_eq!(state.viewing_agent_task_id.as_deref(), Some("a"));
        assert_eq!(state.view_selection_mode, ViewSelectionMode::ViewingAgent);
        assert_eq!(retain_of(&store, "a"), Some(true));
    }

    /// Maps to: CC `:62` — `if (!needsRetain && !needsView && !switching)
    /// return prev`. Re-entering the same already-retained view is a no-op, so
    /// it must not bump the store revision.
    #[test]
    fn re_entering_the_same_view_is_a_same_like_official_guard() {
        let store = store_with(vec![("a", local_agent("a", "running"))]);
        enter_teammate_view("a", &store);
        let settled = store.revision();

        enter_teammate_view("a", &store);

        assert_eq!(store.revision(), settled);
    }

    /// Maps to: CC `:52-56` + `:67` — switching releases the previous agent
    /// back to stub form while retaining the new one.
    #[test]
    fn switching_releases_the_previous_agent_like_official() {
        let store = store_with(vec![
            ("a", local_agent("a", "running")),
            ("b", local_agent("b", "running")),
        ]);
        enter_teammate_view("a", &store);

        enter_teammate_view("b", &store);

        assert_eq!(retain_of(&store, "a"), Some(false), "previous is released");
        assert_eq!(retain_of(&store, "b"), Some(true));
        assert_eq!(store.get().viewing_agent_task_id.as_deref(), Some("b"));
    }

    /// Maps to: CC `:96-101` — with nothing viewed and the mode already
    /// 'none', exiting returns `prev`.
    #[test]
    fn exiting_with_nothing_viewed_is_a_same_like_official_guard() {
        let store = store_with(Vec::new());
        let baseline = store.revision();

        exit_teammate_view(&store);

        assert_eq!(store.revision(), baseline);
    }

    /// Maps to: CC `:103-108` — exiting a retained agent releases it and a
    /// terminal one gets the grace deadline rather than immediate eviction.
    #[test]
    fn exiting_releases_a_retained_agent_and_grace_windows_terminal_ones() {
        let store = store_with(vec![("a", local_agent("a", "completed"))]);
        enter_teammate_view("a", &store);

        exit_teammate_view(&store);

        let state = store.get();
        assert!(state.viewing_agent_task_id.is_none());
        assert_eq!(state.view_selection_mode, ViewSelectionMode::None);
        assert_eq!(retain_of(&store, "a"), Some(false));
        match state.tasks.get("a").expect("task survives").as_ref() {
            TaskState::Other(task) => assert!(
                task.evict_after.is_some_and(|deadline| deadline > now_ms()),
                "a terminal release schedules the panel grace window"
            ),
            _ => panic!("expected the Other stub"),
        }
    }

    /// Maps to: CC `:120-123` — running tasks are aborted by the caller and
    /// the state is left untouched.
    #[test]
    fn stopping_a_running_agent_defers_to_the_caller_without_touching_state() {
        let store = store_with(vec![("a", local_agent("a", "running"))]);
        let baseline = store.revision();

        let must_stop = stop_or_dismiss_agent("a", &store);

        assert!(must_stop, "caller is told to stop the task");
        assert_eq!(store.revision(), baseline, "CC returns prev on this branch");
    }

    /// Maps to: CC `:125-139` — a terminal agent is dismissed with
    /// `evictAfter: 0`, and dismissing the viewed one also exits to leader.
    #[test]
    fn dismissing_the_viewed_terminal_agent_evicts_now_and_exits_to_leader() {
        let store = store_with(vec![("a", local_agent("a", "completed"))]);
        enter_teammate_view("a", &store);

        let must_stop = stop_or_dismiss_agent("a", &store);

        assert!(!must_stop);
        let state = store.get();
        assert!(state.viewing_agent_task_id.is_none());
        assert_eq!(state.view_selection_mode, ViewSelectionMode::None);
        match state.tasks.get("a").expect("task survives").as_ref() {
            TaskState::Other(task) => assert_eq!(task.evict_after, Some(0)),
            _ => panic!("expected the Other stub"),
        }
    }

    /// Maps to: CC `:118-119` + `:124` — non-local_agent tasks and
    /// already-dismissed ones are both `return prev`.
    #[test]
    fn non_local_agent_and_already_dismissed_are_both_same() {
        let store = store_with(vec![("a", local_agent("a", "completed"))]);
        let baseline = store.revision();

        assert!(!stop_or_dismiss_agent("missing", &store));
        assert_eq!(store.revision(), baseline);

        assert!(!stop_or_dismiss_agent("a", &store));
        let after_dismiss = store.revision();
        assert!(!stop_or_dismiss_agent("a", &store));
        assert_eq!(store.revision(), after_dismiss, "second dismiss is a Same");
    }
}
