//! Backgrounding the main session query.
//!
//! Maps to: CC `tasks/LocalMainSessionTask.ts:1-479`.
//!
//! When the user backgrounds the running query, the session keeps running
//! detached, the UI clears to a fresh prompt, and a notification is enqueued
//! when the query completes. Main-session tasks reuse the `LocalAgentTaskState`
//! structure with `agentType: 'main-session'` (CC `:54-57`), so this module
//! builds on the local_agent registry + AppState mirror.
//!
//! SEAM (driver missing): `startBackgroundSession` (CC `:338-479`) spawns an
//! independent `query()` loop feeding progress into the task — its caller is
//! the REPL backgrounding action (CC `screens/REPL.tsx:3403`), which the Rust
//! REPL does not own yet, so the query-driving loop (and its private
//! `ToolActivity`/`MAX_RECENT_ACTIVITIES` helpers, CC `:324-330`) is not
//! ported. The state machine, notification, and foreground flip land first.

use crate::constants::xml::{
    OUTPUT_FILE_TAG, STATUS_TAG, SUMMARY_TAG, TASK_ID_TAG, TASK_NOTIFICATION_TAG, TOOL_USE_ID_TAG,
};
use crate::state::store::{AppStore, UpdateDecision};
use crate::tasks::local_agent_task::{self, LocalAgentTaskState};
use crate::tool::AbortController;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use std::sync::Arc;

/// Maps to: CC `LocalMainSessionTask.ts:62-67` `DEFAULT_MAIN_SESSION_AGENT` —
/// the fallback definition when no `--agent` definition is provided.
fn default_main_session_agent() -> AgentDefinition {
    let mut agent = AgentDefinition::new(
        "main-session",
        "Main session query",
        AgentDefinitionSource::UserSettings,
    );
    // CC `getSystemPrompt: () => ''`.
    agent.system_prompt = Some(String::new());
    agent
}

const TASK_ID_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Maps to: CC `LocalMainSessionTask.ts:73-82` `generateMainSessionTaskId` —
/// 's' prefix distinguishes main-session tasks from agent tasks ('a').
pub fn generate_main_session_task_id() -> String {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).expect("OS randomness is required for secure task IDs");
    let mut id = String::with_capacity(9);
    id.push('s');
    for byte in random {
        id.push(TASK_ID_ALPHABET[byte as usize % TASK_ID_ALPHABET.len()] as char);
    }
    id
}

/// CC `registerMainSessionTask` returns `{ taskId, abortSignal }`; the Rust
/// [`AbortController`] carries both halves.
#[derive(Clone, Debug)]
pub struct MainSessionTaskRegistration {
    pub task_id: String,
    pub abort_controller: AbortController,
}

/// Maps to: CC `LocalMainSessionTask.ts:94-162` `registerMainSessionTask`.
///
/// CC `:116-122` also registers a process-exit cleanup that removes the
/// AppState entry; the cleanup registry is not wired for local_agent-family
/// tasks (same seam as LocalAgentTask's `unregisterCleanup`).
pub fn register_main_session_task(
    description: &str,
    root_store: Option<AppStore>,
    main_thread_agent_definition: Option<AgentDefinition>,
    existing_abort_controller: Option<AbortController>,
) -> MainSessionTaskRegistration {
    let task_id = generate_main_session_task_id();

    // CC `:107-110` — link output to an isolated per-task transcript file
    // (same layout as sub-agents), NOT the main session transcript: writing
    // there from a background query after /clear would corrupt the post-clear
    // conversation. The isolated path lets this task survive /clear via the
    // symlink re-link in clearConversation.
    let transcript_path = crate::utils::session_storage::get_agent_transcript_path(&task_id);
    let output_file =
        crate::utils::task::disk_output::init_task_output_as_symlink(&task_id, transcript_path)
            .unwrap_or_else(|_| crate::utils::task::disk_output::get_task_output_path(&task_id))
            .display()
            .to_string();

    // CC `:112-114` — reuse the active query's controller when provided, so
    // aborting the task aborts the actual query.
    let abort_controller = existing_abort_controller.unwrap_or_default();
    // CC `:124-125`.
    let selected_agent = main_thread_agent_definition.unwrap_or_else(default_main_session_agent);

    // CC `:128-145` — LocalAgentTaskState with agentType 'main-session',
    // already backgrounded (`isBackgrounded: true`, `:141`).
    let state = LocalAgentTaskState {
        task_id: task_id.clone(),
        task_type: "local_agent".to_string(),
        status: "running".to_string(),
        agent_id: task_id.clone(),
        prompt: description.to_string(),
        agent_type: "main-session".to_string(),
        description: description.to_string(),
        model: None,
        selected_agent: Some(selected_agent),
        error: None,
        result: None,
        progress: None,
        retrieved: false,
        messages: Vec::new(),
        last_reported_tool_count: 0,
        last_reported_token_count: 0,
        is_backgrounded: true,
        pending_messages: Vec::new(),
        retain: false,
        disk_loaded: false,
        start_time_ms: crate::utils::task::framework::now_ms(),
        end_time_ms: None,
        tool_use_id: None,
        output_file,
        abort_controller: abort_controller.clone(),
        notified: false,
        evict_after: None,
    };
    // CC `:150` `registerTask(taskState, setAppState)`.
    local_agent_task::insert_task_state(state, root_store);

    MainSessionTaskRegistration {
        task_id,
        abort_controller,
    }
}

/// Maps to: CC `LocalMainSessionTask.ts:168-219` `completeMainSessionTask` —
/// called when the backgrounded query finishes.
pub fn complete_main_session_task(task_id: &str, success: bool) {
    // CC `:173-174` — non-running no-ops leave wasBackgrounded true so the
    // notification CAS still runs (and is suppressed by `notified`).
    let mut was_backgrounded = true;
    let mut tool_use_id: Option<String> = None;
    local_agent_task::update_task_state_and_mirror(task_id, |task| {
        if task.status != "running" {
            return;
        }
        was_backgrounded = task.is_backgrounded;
        tool_use_id = task.tool_use_id.clone();
        task.status = if success { "completed" } else { "failed" }.to_string();
        task.end_time_ms = Some(crate::utils::task::framework::now_ms());
        // CC `:191` — keep only the last accumulated message.
        if task.messages.len() > 1 {
            let last = task.messages.len() - 1;
            task.messages = task.messages.split_off(last);
        }
    });

    // CC `:195` `void evictTaskOutput(taskId)` not performed. CC DOES call
    // it on both families — local_agent terminal transitions
    // (LocalAgentTask.tsx:386/:532/:562) and main-session (:195) — so this is
    // NOT a CC-precedent omission: it is justified only by the Rust
    // `evict_task_output` being a no-op stub (utils/task/disk_output.rs:
    // `Ok(())`), so the call would do nothing. Revisit when the stub gains a
    // body.

    if was_backgrounded {
        // CC `:199-206` — still backgrounded: enqueue the notification.
        enqueue_main_session_notification(
            task_id,
            "Background session",
            success,
            tool_use_id.as_deref(),
        );
    } else {
        // CC `:207-218` — foregrounded: user is watching, no XML
        // notification, but set notified so eviction guards pass.
        // SEAM (sdk queue missing): CC also emits `emitTaskTerminatedSdk`
        // (`utils/sdkEventQueue.ts`), which has no Rust port.
        local_agent_task::update_task_state_and_mirror(task_id, |task| {
            task.notified = true;
        });
    }
}

/// Maps to: CC `LocalMainSessionTask.ts:224-263`
/// `enqueueMainSessionNotification`. Unlike LocalAgentTask's
/// `enqueueAgentNotification`, CC does NOT abort speculation here.
fn enqueue_main_session_notification(
    task_id: &str,
    description: &str,
    success: bool,
    tool_use_id: Option<&str>,
) {
    // CC `:231-243` — atomic check-and-set on notified.
    let should_enqueue = local_agent_task::update_task_state_and_mirror(task_id, |task| {
        if task.notified {
            false
        } else {
            task.notified = true;
            true
        }
    });
    if should_enqueue != Some(true) {
        return;
    }

    let status = if success { "completed" } else { "failed" };
    // CC `:245-248`.
    let summary = format!("Background session \"{description}\" {status}");
    let tool_use_id_line = tool_use_id
        .map(|id| format!("\n<{TOOL_USE_ID_TAG}>{id}</{TOOL_USE_ID_TAG}>"))
        .unwrap_or_default();
    let output_path = crate::utils::task::disk_output::get_task_output_path(task_id)
        .display()
        .to_string();
    // CC `:255-260`.
    let message = format!(
        "<{TASK_NOTIFICATION_TAG}>\n<{TASK_ID_TAG}>{task_id}</{TASK_ID_TAG}>{tool_use_id_line}\n<{OUTPUT_FILE_TAG}>{output_path}</{OUTPUT_FILE_TAG}>\n<{STATUS_TAG}>{status}</{STATUS_TAG}>\n<{SUMMARY_TAG}>{summary}</{SUMMARY_TAG}>\n</{TASK_NOTIFICATION_TAG}>"
    );
    local_agent_task::enqueue_agent_notification_xml(message);
}

/// Maps to: CC `LocalMainSessionTask.ts:270-302` `foregroundMainSessionTask`
/// — mark the task foregrounded so its output appears in the main view; the
/// background query keeps running. Returns the task's accumulated messages
/// (read from the registry — the Rust AppState `Other` stub carries none), or
/// `None` when the id is not a local_agent entry.
pub fn foreground_main_session_task(
    task_id: &str,
    store: &AppStore,
) -> Option<Vec<crate::types::message::Message>> {
    use crate::state::app_state_store::TaskState;

    // CC `:282` — capture the accumulated messages before the flip.
    let messages = local_agent_task::get_local_agent_task(task_id).map(|task| task.messages);

    let mut restored_prev: Option<String> = None;
    let flipped = store.set_state(|prev| {
        let is_local_agent_at = |id: &str| {
            prev.tasks.get(id).is_some_and(|task| {
                matches!(
                    task.as_ref(),
                    TaskState::Other(other) if other.task_type == "local_agent"
                )
            })
        };
        // CC `:277-280` — `if (!task || task.type !== 'local_agent') return prev`.
        if !is_local_agent_at(task_id) {
            return UpdateDecision::Same(false);
        }
        // CC `:284-288` — restore the previous foregrounded task to
        // background when it is a different local_agent task.
        let prev_id = prev.foregrounded_task_id.clone();
        let restore_prev = prev_id
            .as_deref()
            .is_some_and(|pid| pid != task_id && is_local_agent_at(pid));
        let mut next = (**prev).clone();
        // CC `:292` — `foregroundedTaskId: taskId`.
        next.foregrounded_task_id = Some(task_id.to_string());
        let tasks = Arc::make_mut(&mut next.tasks);
        // CC `:295` — `[prevId]: {...prevTask, isBackgrounded: true}`.
        if restore_prev {
            if let Some(entry) = prev_id.as_deref().and_then(|pid| tasks.get_mut(pid))
                && let TaskState::Other(other) = Arc::make_mut(entry)
            {
                other.is_backgrounded = Some(true);
            }
            restored_prev = prev_id;
        }
        // CC `:296` — `[taskId]: {...task, isBackgrounded: false}`.
        if let Some(entry) = tasks.get_mut(task_id)
            && let TaskState::Other(other) = Arc::make_mut(entry)
        {
            other.is_backgrounded = Some(false);
        }
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: true,
        }
    });

    if !flipped {
        return None;
    }
    // Rust-only dual-registry sync: the AppState flip above is CC's entire
    // write; the registry copy must agree or the next lifecycle mirror would
    // revert it. Still required after the register_task re-register merge
    // (CC framework.ts:87-97): the merge preserves only `retain` on the
    // AppState side — `isBackgrounded` is NOT merge-listed, so a mirror from
    // an unsynced registry would overwrite the flip with the stale value.
    // The mirrors these two updates trigger re-write the same values the
    // flip already installed (value-identical no-ops beyond the notify).
    if let Some(prev_id) = restored_prev {
        local_agent_task::update_task_state_and_mirror(&prev_id, |task| {
            task.is_backgrounded = true;
        });
    }
    local_agent_task::update_task_state_and_mirror(task_id, |task| {
        task.is_backgrounded = false;
    });
    messages
}

/// Maps to: CC `LocalMainSessionTask.ts:307-322` `isMainSessionTask` — a
/// local_agent task whose agentType is 'main-session' (vs a regular agent).
pub fn is_main_session_task(task: &LocalAgentTaskState) -> bool {
    task.task_type == "local_agent" && task.agent_type == "main-session"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::{AppState, TaskState};
    use crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK;

    fn store() -> AppStore {
        AppStore::new(AppState::default(), None)
    }

    fn mirror_entry(
        store: &AppStore,
        task_id: &str,
    ) -> crate::state::app_state_store::TaskStateOther {
        let state = store.get();
        match state.tasks.get(task_id).map(|task| task.as_ref().clone()) {
            Some(TaskState::Other(other)) => other,
            other => panic!("expected an Other mirror entry for {task_id}, got {other:?}"),
        }
    }

    #[test]
    fn register_complete_and_notification_follow_official_lifecycle() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = store();

        let registration =
            register_main_session_task("finish the refactor", Some(store.clone()), None, None);
        assert!(registration.task_id.starts_with('s'));
        assert_eq!(registration.task_id.len(), 9);
        let task =
            crate::tasks::local_agent_task::get_local_agent_task(&registration.task_id).unwrap();
        assert!(is_main_session_task(&task));
        assert_eq!(task.agent_type, "main-session");
        assert!(task.is_backgrounded, "CC :141 — already backgrounded");
        assert_eq!(
            task.selected_agent.as_ref().unwrap().when_to_use,
            "Main session query"
        );
        let entry = mirror_entry(&store, &registration.task_id);
        assert_eq!(entry.task_type, "local_agent");
        assert_eq!(entry.status, "running");

        // CC :168-219 — completed backgrounded session enqueues the
        // Background session notification exactly once.
        complete_main_session_task(&registration.task_id, true);
        let entry = mirror_entry(&store, &registration.task_id);
        assert_eq!(entry.status, "completed");
        assert!(entry.notified);
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1
        );
        let queued = crate::utils::message_queue_manager::dequeue(|_| true).unwrap();
        assert!(
            queued
                .value
                .contains("Background session \"Background session\" completed")
        );
        assert!(queued.value.contains(&registration.task_id));

        // Duplicate terminal transition stays suppressed by the CAS.
        complete_main_session_task(&registration.task_id, true);
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            0
        );

        let _ = crate::utils::task::disk_output::cleanup_task_output(&registration.task_id);
        crate::utils::message_queue_manager::clear_command_queue();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[test]
    fn foreground_flip_swaps_backgrounded_state_and_suppresses_notification() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = store();

        let first = register_main_session_task("first", Some(store.clone()), None, None);
        let second = register_main_session_task("second", Some(store.clone()), None, None);

        // CC :270-302 — foreground the first task.
        assert!(foreground_main_session_task(&first.task_id, &store).is_some());
        assert_eq!(
            store.get().foregrounded_task_id.as_deref(),
            Some(first.task_id.as_str())
        );
        assert_eq!(
            mirror_entry(&store, &first.task_id).is_backgrounded,
            Some(false)
        );

        // Foregrounding the second restores the first to background (CC :284-295).
        assert!(foreground_main_session_task(&second.task_id, &store).is_some());
        assert_eq!(
            mirror_entry(&store, &first.task_id).is_backgrounded,
            Some(true)
        );
        assert_eq!(
            mirror_entry(&store, &second.task_id).is_backgrounded,
            Some(false)
        );
        // Registry stays in sync (Rust dual-registry invariant).
        assert!(
            crate::tasks::local_agent_task::get_local_agent_task(&first.task_id)
                .unwrap()
                .is_backgrounded
        );
        assert!(
            !crate::tasks::local_agent_task::get_local_agent_task(&second.task_id)
                .unwrap()
                .is_backgrounded
        );

        // CC :199-218 — a foregrounded completion sets notified WITHOUT an
        // XML notification.
        complete_main_session_task(&second.task_id, false);
        let entry = mirror_entry(&store, &second.task_id);
        assert_eq!(entry.status, "failed");
        assert!(entry.notified);
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            0,
            "foregrounded completion must not enqueue the XML notification"
        );

        // Unknown / non-local_agent ids are rejected (CC :277-280).
        assert!(foreground_main_session_task("missing", &store).is_none());

        for id in [&first.task_id, &second.task_id] {
            let _ = crate::utils::task::disk_output::cleanup_task_output(id);
        }
        crate::utils::message_queue_manager::clear_command_queue();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }
}
