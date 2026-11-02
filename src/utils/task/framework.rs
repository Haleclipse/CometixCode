//! Maps to: CC `utils/task/framework.ts` — AppState.tasks register/update/evict.
//!
//! Live registries (in-process teammate / local agent / shell) remain for
//! abort handles and non-UI state; UI consumers read `AppState.tasks`. Mutations
//! that affect the spinner/footer must call these helpers (or the registry
//! wrappers that mirror into AppState).
//!
//! `update_task_state` gained its first direct consumer with the Dream state
//! machine (`tasks/dream_task.rs` add/complete/fail/kill); teammate and
//! local_agent mutations still go through their registries + the
//! `register_task` mirror, which carries CC's `updateTaskState` call surface.
//! `update_task_on_bound_store` remains caller-less (future wiring).

use crate::state::app_state_store::TaskState;
use crate::state::store::{AppStore, UpdateDecision};
use std::sync::{Arc, Mutex, OnceLock};

/// Maps to: CC `utils/task/framework.ts:28` — grace period for terminal
/// local_agent tasks in the coordinator panel (`PANEL_GRACE_MS = 30_000`).
pub const PANEL_GRACE_MS: u64 = 30_000;

static BOUND_TASK_APP_STORE: OnceLock<Mutex<Option<AppStore>>> = OnceLock::new();

fn bound_store() -> &'static Mutex<Option<AppStore>> {
    BOUND_TASK_APP_STORE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Maps to: CC `Task.ts:27-29` `isTerminalTaskStatus`, applied over the
/// unified `TaskState` variants (each carries its own `status` string).
pub fn is_terminal_task_status(task: &TaskState) -> bool {
    let status = match task {
        TaskState::InProcessTeammate(task) => task.status.as_str(),
        TaskState::LocalShell(task) => task.status.as_str(),
        TaskState::Dream(task) => task.status.as_str(),
        TaskState::Other(task) => task.status.as_str(),
    };
    matches!(status, "completed" | "failed" | "killed")
}

/// Maps to: CC `TaskStateBase.notified` read in `evictTerminalTask`
/// (utils/task/framework.ts:133).
fn task_notified(task: &TaskState) -> bool {
    match task {
        TaskState::InProcessTeammate(task) => task.notified,
        TaskState::LocalShell(task) => task.notified,
        TaskState::Dream(task) => task.notified,
        TaskState::Other(task) => task.notified,
    }
}

/// Bind the UI AppStore so registry mutations can mirror into `AppState.tasks`.
/// Maps to: CC passing `setAppState` into task helpers.
pub fn bind_task_app_store(store: Option<AppStore>) {
    *bound_store().lock().unwrap() = store;
}

pub fn with_bound_task_app_store<R>(f: impl FnOnce(&AppStore) -> R) -> Option<R> {
    let guard = bound_store().lock().unwrap();
    guard.as_ref().map(f)
}

/// Maps to: CC `utils/task/framework.ts:77-117` `registerTask` — the
/// `setAppState` install (:79-99). The double spread
/// `{...prev, tasks: {...prev.tasks, [task.id]: merged}}` is
/// `Arc::make_mut(map) + Arc::new(task)` (fresh map + fresh task identity).
///
/// Re-register merge (CC :87-97): when the existing entry carries `retain`
/// (CC `'retain' in existing` — only `LocalAgentTaskState` has the field; in
/// the Rust union that is an `Other` stub with `retain: Some(_)`), the new
/// state wins EXCEPT `retain`/`startTime`/`messages`/`diskLoaded`/
/// `pendingMessages`, preserved from the existing entry. Of those five the
/// Rust `Other` projection carries only `retain`; the registry-side twin
/// (`tasks/local_agent_task.rs::merge_reregistered_task_state`) preserves
/// all five. This is live, not a no-op: the local_agent mirror always writes
/// `retain: Some(_)` and re-registers on every lifecycle transition, so
/// without the merge a mirror write would clobber the UI-held `retain: true`
/// set by enterTeammateView (`state/teammate_view_helpers.rs`).
///
/// The `task_started` SDK event (:104-116, the `enqueueSdkEvent` call)
/// remains the SDK-queue owner's seam.
pub fn register_task(task: TaskState, app_store: &AppStore) {
    let id = task.id().to_string();
    app_store.replace_with(|state| {
        let map = Arc::make_mut(&mut state.tasks);
        let merged = match (map.get(&id).map(|existing| existing.as_ref()), task) {
            // CC :87-97 — `'retain' in existing` (`Some` = field present).
            (Some(TaskState::Other(existing)), TaskState::Other(mut incoming))
                if existing.retain.is_some() =>
            {
                incoming.retain = existing.retain;
                TaskState::Other(incoming)
            }
            (_, task) => task,
        };
        map.insert(id, Arc::new(merged));
    });
}

/// Maps to: CC `utils/task/framework.ts:48-72` `updateTaskState`.
///
/// The updater receives the store-held `Arc<TaskState>` (CC :58 passes the
/// store-held object) and returns either the same Arc (early-return no-op)
/// or a fresh one. The guard is `Arc::ptr_eq` — exactly CC :59
/// `updated === task` reference identity. (2026-08-02 P4: this retires the
/// former value-equality approximation and its KNOWN DIVERGENCE note; the
/// double-layer `AppState.tasks` Arc makes reference identity expressible.)
pub fn update_task_state(
    task_id: &str,
    app_store: &AppStore,
    updater: impl FnOnce(&Arc<TaskState>) -> Arc<TaskState>,
) {
    app_store.set_state(|prev| {
        // CC framework.ts:55-56 — `if (!task) return prev`.
        let Some(existing) = prev.tasks.get(task_id) else {
            return UpdateDecision::Same(());
        };
        let updated = updater(existing);
        // CC framework.ts:59-63 — same reference → skip the spread so
        // s.tasks subscribers don't re-render on unchanged state.
        if Arc::ptr_eq(&updated, existing) {
            return UpdateDecision::Same(());
        }
        // CC framework.ts:64-70 — `{...prev, tasks: {...prev.tasks,
        // [taskId]: updated}}`.
        let mut next = (**prev).clone();
        Arc::make_mut(&mut next.tasks).insert(task_id.to_string(), updated);
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `utils/task/framework.ts:125-144` `evictTerminalTask`.
///
/// Eagerly evict a terminal task from AppState. The task must be terminal
/// (completed/failed/killed) with `notified: true`; local_agent tasks (the
/// only carriers of `retain`) additionally honor the coordinator-panel grace
/// deadline (`evictAfter ?? Infinity`, [`PANEL_GRACE_MS`]).
pub fn evict_terminal_task(task_id: &str, app_store: &AppStore) {
    app_store.set_state(|prev| {
        // CC :131 — `if (!task) return prev`.
        let Some(task) = prev.tasks.get(task_id) else {
            return UpdateDecision::Same(());
        };
        // CC :132 — `if (!isTerminalTaskStatus(task.status)) return prev`.
        if !is_terminal_task_status(task) {
            return UpdateDecision::Same(());
        }
        // CC :133 — `if (!task.notified) return prev`.
        if !task_notified(task) {
            return UpdateDecision::Same(());
        }
        // CC :138-140 — `'retain' in task && (task.evictAfter ?? Infinity) >
        // Date.now()`. `retain` only exists on LocalAgentTaskState; in the
        // Rust union that is the `Other` stub with `retain: Some(_)`
        // (`None` = field absent → CC's `in` narrowing is false).
        if let TaskState::Other(other) = task.as_ref()
            && other.retain.is_some()
            && other.evict_after.is_none_or(|deadline| deadline > now_ms())
        {
            return UpdateDecision::Same(());
        }
        // CC :141-142 — `const {[taskId]: _, ...remainingTasks} = prev.tasks`.
        let mut next = (**prev).clone();
        Arc::make_mut(&mut next.tasks).remove(task_id);
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Rust-only thin direct removal — no CC framework counterpart symbol. Used
/// only by the registry→AppState mirror when the live registry entry has
/// vanished out from under its AppState snapshot (a Rust-only desync CC
/// cannot express: CC keeps the full TaskState in AppState). Terminal-path
/// eviction must go through [`evict_terminal_task`] (P4, 2026-08-02).
pub fn remove_task(task_id: &str, app_store: &AppStore) {
    app_store.set_state(|prev| {
        if !prev.tasks.contains_key(task_id) {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        Arc::make_mut(&mut next.tasks).remove(task_id);
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
}

/// Mirror helper used when only the bound UI store is available.
pub fn register_task_on_bound_store(task: TaskState) {
    with_bound_task_app_store(|store| register_task(task, store));
}

pub fn update_task_on_bound_store(
    task_id: &str,
    updater: impl FnOnce(&Arc<TaskState>) -> Arc<TaskState>,
) {
    with_bound_task_app_store(|store| update_task_state(task_id, store, updater));
}

pub fn evict_terminal_task_on_bound_store(task_id: &str) {
    with_bound_task_app_store(|store| evict_terminal_task(task_id, store));
}

pub fn remove_task_on_bound_store(task_id: &str) {
    with_bound_task_app_store(|store| remove_task(task_id, store));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::{AppState, TaskStateOther};

    fn other_task(id: &str, status: &str, notified: bool) -> TaskState {
        TaskState::Other(TaskStateOther {
            id: id.to_string(),
            task_type: "local_agent".to_string(),
            status: status.to_string(),
            description: "test".to_string(),
            is_backgrounded: Some(true),
            notified,
            retain: None,
            evict_after: None,
            progress_tool_uses: None,
            progress_tokens: None,
        })
    }

    fn store_with(tasks: Vec<(&str, TaskState)>) -> AppStore {
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| {
            let map = Arc::make_mut(&mut state.tasks);
            for (id, task) in tasks {
                map.insert(id.to_string(), Arc::new(task));
            }
        });
        store
    }

    #[test]
    fn update_task_state_same_arc_early_return_is_ptr_eq_same_like_cc_59() {
        // CC framework.ts:59 — `updated === task` catches an early-return of
        // the SAME reference; a fresh value-equal object would NOT be caught.
        let store = store_with(vec![("t1", other_task("t1", "running", false))]);
        let revision_before = store.revision();
        update_task_state("t1", &store, Arc::clone);
        assert_eq!(
            store.revision(),
            revision_before,
            "same Arc → Same, no install"
        );

        // A fresh Arc with an IDENTICAL value installs + notifies (reference
        // identity, not value equality — the P4 fix over the old predicate).
        update_task_state("t1", &store, |task| Arc::new((**task).clone()));
        assert_eq!(
            store.revision(),
            revision_before + 1,
            "value-equal NEW Arc → Replace like CC's spread"
        );
    }

    #[test]
    fn update_task_state_missing_task_is_same_like_cc_55() {
        let store = store_with(vec![]);
        let revision_before = store.revision();
        update_task_state("missing", &store, Arc::clone);
        assert_eq!(store.revision(), revision_before);
    }

    #[test]
    fn update_task_state_replace_installs_fresh_map_and_task_identity() {
        let store = store_with(vec![("t1", other_task("t1", "running", false))]);
        let map_before = Arc::clone(&store.get().tasks);
        let task_before = Arc::clone(map_before.get("t1").unwrap());
        update_task_state("t1", &store, |task| {
            let mut next = (**task).clone();
            if let TaskState::Other(other) = &mut next {
                other.status = "completed".to_string();
            }
            Arc::new(next)
        });
        let state = store.get();
        assert!(
            !Arc::ptr_eq(&state.tasks, &map_before),
            "fresh map identity"
        );
        assert!(
            !Arc::ptr_eq(state.tasks.get("t1").unwrap(), &task_before),
            "fresh per-task identity"
        );
    }

    #[test]
    fn register_task_reregister_preserves_existing_retain_like_cc_87_to_97() {
        // CC framework.ts:87-97 — the merge carries forward the UI-held
        // fields; of the five, the Rust Other stub carries only `retain`.
        let mut retained = other_task("t1", "running", false);
        if let TaskState::Other(other) = &mut retained {
            other.retain = Some(true);
        }
        let store = store_with(vec![("t1", retained)]);

        // A mirror re-register with retain: Some(false) must NOT clobber the
        // UI-held Some(true); everything else takes the new value.
        let mut incoming = other_task("t1", "completed", true);
        if let TaskState::Other(other) = &mut incoming {
            other.retain = Some(false);
            other.progress_tool_uses = Some(3);
        }
        register_task(incoming, &store);
        let state = store.get();
        let TaskState::Other(other) = state.tasks.get("t1").unwrap().as_ref() else {
            panic!("expected Other");
        };
        assert_eq!(other.retain, Some(true), "merge-listed field preserved");
        assert_eq!(other.status, "completed", "new state wins elsewhere");
        assert!(other.notified);
        assert_eq!(
            other.progress_tool_uses,
            Some(3),
            "progress is NOT merge-listed — the new value wins"
        );

        // CC :88 — `'retain' in existing` false (None = field absent): the
        // new state replaces wholesale.
        let store = store_with(vec![("t2", other_task("t2", "running", false))]);
        let mut incoming = other_task("t2", "running", false);
        if let TaskState::Other(other) = &mut incoming {
            other.retain = Some(false);
        }
        register_task(incoming, &store);
        let TaskState::Other(other) = store.get().tasks.get("t2").unwrap().as_ref().clone() else {
            panic!("expected Other");
        };
        assert_eq!(
            other.retain,
            Some(false),
            "no merge without existing retain"
        );
    }

    #[test]
    fn evict_terminal_task_guards_match_cc_131_to_140() {
        // :132 — non-terminal stays.
        let store = store_with(vec![("t1", other_task("t1", "running", true))]);
        evict_terminal_task("t1", &store);
        assert!(store.get().tasks.contains_key("t1"));

        // :133 — terminal but not notified stays.
        let store = store_with(vec![("t1", other_task("t1", "completed", false))]);
        evict_terminal_task("t1", &store);
        assert!(store.get().tasks.contains_key("t1"));

        // :141-142 — terminal + notified is removed.
        let store = store_with(vec![("t1", other_task("t1", "completed", true))]);
        evict_terminal_task("t1", &store);
        assert!(!store.get().tasks.contains_key("t1"));

        // :131 — missing id: Same (no install).
        let revision_before = store.revision();
        evict_terminal_task("t1", &store);
        assert_eq!(store.revision(), revision_before);
    }

    #[test]
    fn evict_terminal_task_honors_retain_grace_window_like_cc_138() {
        let make = |evict_after: Option<u64>| {
            let mut task = other_task("t1", "killed", true);
            if let TaskState::Other(other) = &mut task {
                other.retain = Some(false);
                other.evict_after = evict_after;
            }
            task
        };
        // `evictAfter ?? Infinity` — retain present, no deadline → blocked.
        let store = store_with(vec![("t1", make(None))]);
        evict_terminal_task("t1", &store);
        assert!(store.get().tasks.contains_key("t1"));

        // Future deadline → blocked.
        let store = store_with(vec![("t1", make(Some(now_ms() + PANEL_GRACE_MS)))]);
        evict_terminal_task("t1", &store);
        assert!(store.get().tasks.contains_key("t1"));

        // Past deadline → evicted.
        let store = store_with(vec![("t1", make(Some(now_ms().saturating_sub(1))))]);
        evict_terminal_task("t1", &store);
        assert!(!store.get().tasks.contains_key("t1"));
    }
}
