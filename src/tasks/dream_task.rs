//! Background task entry for auto-dream (memory consolidation subagent).
//!
//! Maps to: CC `tasks/DreamTask/DreamTask.ts:1-157`.
//!
//! Live surface today: the turn-duration transcript line projects Dream
//! entries into its pill label (`components/messages/turn_duration_message.rs`
//! → `PillTask::Dream`), and TaskStopTool / the SDK stop_task dispatch can
//! kill a dream (`tasks/stop_task.rs` "dream" arm → [`kill_dream_task`]).
//!
//! @nyan: the producer is `services/auto_dream::run_consolidation` (fork +
//! onMessage feed), driven by the idle trigger in `query/stop_hooks.rs`. The
//! task carries a [`DreamLockLease`] so kill/fail roll the consolidation lock
//! back (CC `rollbackConsolidationLock`) without the producer's cooperation.

use crate::state::app_state_store::TaskState;
use crate::state::store::AppStore;
use crate::task::{TaskType, generate_task_id};
use crate::tool::AbortController;
use crate::utils::task::framework::{register_task, update_task_state};
use std::sync::Arc;

/// Maps to: CC `DreamTask.ts:12` — keep only the N most recent turns for live
/// display.
const MAX_TURNS: usize = 30;

/// Maps to: CC `DreamTask.ts:15-18` — a single assistant turn from the dream
/// agent, tool uses collapsed to a count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DreamTurn {
    pub text: String,
    pub tool_use_count: usize,
}

/// Maps to: CC `DreamTask.ts:20-23` — no phase detection; flip from
/// `starting` to `updating` when the first Edit/Write tool_use lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DreamPhase {
    Starting,
    Updating,
}

impl DreamPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Updating => "updating",
        }
    }
}

/// Maps to: CC `DreamTask.ts:25-41` `DreamTaskState` (TaskStateBase subset the
/// Rust union carries + the dream-specific fields).
#[derive(Clone, Debug, PartialEq)]
pub struct DreamTaskState {
    pub id: String,
    pub task_type: String,
    pub status: String,
    pub description: String,
    pub notified: bool,
    /// Maps to CC `TaskStateBase.startTime` (epoch ms).
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub phase: DreamPhase,
    pub sessions_reviewing: usize,
    /// Paths observed in Edit/Write tool_use blocks via onMessage — an
    /// INCOMPLETE reflection ("at least these were touched"), CC `:29-35`.
    pub files_touched: Vec<String>,
    /// Assistant text responses, tool uses collapsed; prompt NOT included.
    pub turns: Vec<DreamTurn>,
    pub abort_controller: Option<AbortController>,
    /// Stashed so kill can rewind the consolidation-lock mtime (CC `:39-40`).
    /// Epoch ms (CC stores `mtimeMs`).
    pub prior_mtime: u64,
    /// @nyan: lease identity needed to roll the consolidation lock back from
    /// kill/fail without holding the `ConsolidationLock` value. `None` for a
    /// task registered without a lock (no rollback is performed).
    pub lock: Option<DreamLockLease>,
}

/// @nyan: identifies one `.consolidate-lock` lease for lease-checked rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DreamLockLease {
    pub memory_dir: std::path::PathBuf,
    /// `None` = the lock file did not exist before this run.
    pub prior_mtime_ms: Option<u64>,
    pub start_mtime_ms: u64,
}

impl DreamLockLease {
    fn rollback(&self) {
        if let Err(error) = crate::services::auto_dream::lock::ConsolidationLock::rollback_lease(
            &self.memory_dir,
            self.prior_mtime_ms,
            self.start_mtime_ms,
        ) {
            tracing::warn!(
                target: "nyan::memory",
                error = %error,
                "dream_lock_rollback_failed"
            );
        }
    }
}

impl Eq for DreamTaskState {}

/// Maps to: CC `DreamTask.ts:43-50` `isDreamTask` — the Rust enum discriminant
/// supplies the runtime guard; this preserves the source name for consumers.
pub fn is_dream_task(task: &TaskState) -> Option<&DreamTaskState> {
    match task {
        TaskState::Dream(task) => Some(task),
        _ => None,
    }
}

/// Maps to: CC `DreamTask.ts:52-74` `registerDreamTask`.
pub fn register_dream_task(
    store: &AppStore,
    sessions_reviewing: usize,
    prior_mtime: u64,
    abort_controller: AbortController,
    lock: Option<DreamLockLease>,
) -> String {
    let id = generate_task_id(TaskType::Dream);
    let task = DreamTaskState {
        // CC `:62` createTaskStateBase(id, 'dream', 'dreaming') + `:64`
        // status: 'running'.
        id: id.clone(),
        task_type: "dream".to_string(),
        status: "running".to_string(),
        description: "dreaming".to_string(),
        notified: false,
        start_time: crate::utils::task::framework::now_ms(),
        end_time: None,
        phase: DreamPhase::Starting,
        sessions_reviewing,
        files_touched: Vec::new(),
        turns: Vec::new(),
        abort_controller: Some(abort_controller),
        prior_mtime,
        lock,
    };
    register_task(TaskState::Dream(task), store);
    id
}

/// Maps to: CC `DreamTask.ts:76-104` `addDreamTurn`.
pub fn add_dream_turn(task_id: &str, turn: DreamTurn, touched_paths: &[String], store: &AppStore) {
    update_task_state(task_id, store, |existing| {
        let TaskState::Dream(task) = existing.as_ref() else {
            return Arc::clone(existing);
        };
        // CC `:83-84` — `const seen = new Set(task.filesTouched);
        // touchedPaths.filter(p => !seen.has(p) && seen.add(p))`: the Set
        // grows during the filter, so duplicates WITHIN the batch dedupe too.
        let mut seen: std::collections::HashSet<&str> = task
            .files_touched
            .iter()
            .map(|path| path.as_str())
            .collect();
        let new_touched: Vec<String> = touched_paths
            .iter()
            .filter(|path| seen.insert(path.as_str()))
            .cloned()
            .collect();
        // CC `:86-93` — skip the update entirely if the turn is empty AND
        // nothing new was touched (the same-reference return is the ptr_eq
        // no-op).
        if turn.text.is_empty() && turn.tool_use_count == 0 && new_touched.is_empty() {
            return Arc::clone(existing);
        }
        let mut next = task.clone();
        if !new_touched.is_empty() {
            next.phase = DreamPhase::Updating;
            next.files_touched.extend(new_touched);
        }
        // CC `:101` — `task.turns.slice(-(MAX_TURNS - 1)).concat(turn)`.
        let keep_from = next.turns.len().saturating_sub(MAX_TURNS - 1);
        next.turns.drain(..keep_from);
        next.turns.push(turn);
        Arc::new(TaskState::Dream(next))
    });
}

/// Maps to: CC `DreamTask.ts:106-120` `completeDreamTask` — `notified: true`
/// immediately: dream has no model-facing notification path (UI-only), and
/// eviction requires terminal + notified.
///
/// @nyan: only a still-running task transitions; a kill that lands between the
/// fork's last message and its completion must not be reported as success.
pub fn complete_dream_task(task_id: &str, store: &AppStore) {
    update_task_state(task_id, store, |existing| {
        let TaskState::Dream(task) = existing.as_ref() else {
            return Arc::clone(existing);
        };
        if task.status != "running" {
            return Arc::clone(existing);
        }
        let mut next = task.clone();
        next.status = "completed".to_string();
        next.end_time = Some(crate::utils::task::framework::now_ms());
        next.notified = true;
        next.abort_controller = None;
        Arc::new(TaskState::Dream(next))
    });
}

/// Maps to: CC `DreamTask.ts:122-130` `failDreamTask`.
///
/// @nyan: only a still-running task transitions (a killed one stays killed),
/// and the consolidation lock is rolled back so the next idle pass can retry.
pub fn fail_dream_task(task_id: &str, store: &AppStore) {
    let mut lease = None;
    update_task_state(task_id, store, |existing| {
        let TaskState::Dream(task) = existing.as_ref() else {
            return Arc::clone(existing);
        };
        if task.status != "running" {
            return Arc::clone(existing);
        }
        lease = task.lock.clone();
        let mut next = task.clone();
        next.status = "failed".to_string();
        next.end_time = Some(crate::utils::task::framework::now_ms());
        next.notified = true;
        next.abort_controller = None;
        Arc::new(TaskState::Dream(next))
    });
    if let Some(lease) = lease {
        lease.rollback();
    }
}

/// Maps to: CC `DreamTask.ts:132-157` `DreamTask.kill`.
///
/// Returns true when a running task transitioned to killed.
pub fn kill_dream_task(task_id: &str, store: &AppStore) -> bool {
    let mut killed = false;
    let mut lease = None;
    update_task_state(task_id, store, |existing| {
        let TaskState::Dream(task) = existing.as_ref() else {
            return Arc::clone(existing);
        };
        // CC `:139` — `if (task.status !== 'running') return task`.
        if task.status != "running" {
            return Arc::clone(existing);
        }
        // CC `:140` — abort inside the updater (atomic-flag side effect).
        if let Some(abort) = &task.abort_controller {
            abort.abort();
        }
        killed = true;
        lease = task.lock.clone();
        let mut next = task.clone();
        next.status = "killed".to_string();
        next.end_time = Some(crate::utils::task::framework::now_ms());
        next.notified = true;
        next.abort_controller = None;
        Arc::new(TaskState::Dream(next))
    });
    // CC `:153-155` `rollbackConsolidationLock(priorMtime)`. Lease-checked and
    // idempotent: the fork's own cleanup path rolls back too and the second
    // call is a no-op.
    if let Some(lease) = lease {
        lease.rollback();
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;

    fn store() -> AppStore {
        AppStore::new(AppState::default(), None)
    }

    fn dream(store: &AppStore, task_id: &str) -> DreamTaskState {
        let state = store.get();
        match state.tasks.get(task_id).map(|task| task.as_ref().clone()) {
            Some(TaskState::Dream(task)) => task,
            other => panic!("expected a Dream entry for {task_id}, got {other:?}"),
        }
    }

    #[test]
    fn register_creates_running_starting_dream_with_official_base() {
        let store = store();
        let abort = AbortController::default();
        let id = register_dream_task(&store, 3, 42, abort, None);
        assert!(id.starts_with('d'));
        assert_eq!(id.len(), 9);
        let task = dream(&store, &id);
        assert_eq!(task.status, "running");
        assert_eq!(task.description, "dreaming");
        assert_eq!(task.phase, DreamPhase::Starting);
        assert_eq!(task.sessions_reviewing, 3);
        assert_eq!(task.prior_mtime, 42);
        assert!(!task.notified);
    }

    /// A kill that lands between the fork's final message and its completion
    /// is terminal: `complete_dream_task` must not overwrite it.
    #[test]
    fn complete_does_not_resurrect_a_killed_task() {
        let store = store();
        let id = register_dream_task(&store, 0, 0, AbortController::default(), None);
        assert!(kill_dream_task(&id, &store));
        complete_dream_task(&id, &store);
        assert_eq!(dream(&store, &id).status, "killed");
    }

    #[test]
    fn add_dream_turn_flips_phase_dedupes_paths_and_caps_turns() {
        let store = store();
        let id = register_dream_task(&store, 1, 0, AbortController::default(), None);

        // CC :86-93 — pure no-op turn installs nothing (same reference).
        let revision = store.revision();
        add_dream_turn(
            &id,
            DreamTurn {
                text: String::new(),
                tool_use_count: 0,
            },
            &[],
            &store,
        );
        assert_eq!(store.revision(), revision, "empty turn is a ptr_eq no-op");

        add_dream_turn(
            &id,
            DreamTurn {
                text: "consolidating".to_string(),
                tool_use_count: 2,
            },
            &["/tmp/a.md".to_string(), "/tmp/a.md".to_string()],
            &store,
        );
        let task = dream(&store, &id);
        assert_eq!(task.phase, DreamPhase::Updating);
        assert_eq!(task.files_touched, vec!["/tmp/a.md".to_string()]);
        assert_eq!(task.turns.len(), 1);

        // CC :101 — turns capped at MAX_TURNS.
        for index in 0..40 {
            add_dream_turn(
                &id,
                DreamTurn {
                    text: format!("turn {index}"),
                    tool_use_count: 0,
                },
                &[],
                &store,
            );
        }
        let task = dream(&store, &id);
        assert_eq!(task.turns.len(), MAX_TURNS);
        assert_eq!(task.turns.last().unwrap().text, "turn 39");
    }

    #[test]
    fn terminal_transitions_set_notified_and_kill_aborts_running_only() {
        let store = store();
        let abort = AbortController::default();
        let id = register_dream_task(&store, 1, 7, abort.clone(), None);

        assert!(kill_dream_task(&id, &store));
        assert!(abort.is_aborted());
        let task = dream(&store, &id);
        assert_eq!(task.status, "killed");
        assert!(task.notified);
        assert!(task.abort_controller.is_none());
        // CC :139 — already terminal: no-op.
        assert!(!kill_dream_task(&id, &store));

        let id = register_dream_task(&store, 1, 0, AbortController::default(), None);
        complete_dream_task(&id, &store);
        let task = dream(&store, &id);
        assert_eq!(task.status, "completed");
        assert!(task.notified);

        let id = register_dream_task(&store, 1, 0, AbortController::default(), None);
        fail_dream_task(&id, &store);
        let task = dream(&store, &id);
        assert_eq!(task.status, "failed");
        assert!(task.notified);
    }

    #[test]
    fn kill_and_fail_roll_back_the_consolidation_lock_lease() {
        use crate::services::auto_dream::lock::ConsolidationLock;
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir =
            std::env::temp_dir().join(format!("nyan-dream-kill-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = ConsolidationLock::path(&dir);

        // No prior lock: kill removes the lease.
        let lease = ConsolidationLock::acquire(&dir).unwrap();
        let lease = DreamLockLease {
            memory_dir: dir.clone(),
            prior_mtime_ms: lease.prior_mtime_ms(),
            start_mtime_ms: lease.start_mtime_ms(),
        };
        let store = store();
        let id = register_dream_task(
            &store,
            1,
            0,
            AbortController::default(),
            Some(lease.clone()),
        );
        assert!(kill_dream_task(&id, &store));
        assert!(!lock_path.exists(), "kill restores the pre-run state");
        // Idempotent: the producer's own rollback after the kill is a no-op.
        lease.rollback();
        assert!(!lock_path.exists());

        // Prior lock existed: fail restores its mtime (retry stays possible).
        std::fs::write(&lock_path, "").unwrap();
        let prior = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        let prior_ms = prior
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        filetime_set(&lock_path, prior_ms);
        let lock = ConsolidationLock::acquire(&dir).unwrap();
        let lease = DreamLockLease {
            memory_dir: dir.clone(),
            prior_mtime_ms: lock.prior_mtime_ms(),
            start_mtime_ms: lock.start_mtime_ms(),
        };
        std::mem::forget(lock);
        let id = register_dream_task(&store, 1, 0, AbortController::default(), Some(lease));
        fail_dream_task(&id, &store);
        let restored = std::fs::metadata(&lock_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(
            restored.abs_diff(prior_ms) <= 1_000,
            "{restored} vs {prior_ms}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Set an mtime without pulling the lock module's private helper in.
    fn filetime_set(path: &std::path::Path, milliseconds: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(milliseconds);
        file.set_modified(time).unwrap();
    }
}
