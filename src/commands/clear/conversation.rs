//! Conversation clearing utility.
//!
//! Maps to: CC `commands/clear/conversation.ts`.
//! Heavier than [`super::caches`]; keep lazy-called from `/clear` only.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::state::app_state_store::{AppState, TaskState};
use crate::state::store::AppStore;
use crate::types::message::RenderableMessage;

/// Result of [`clear_conversation`] for the REPL to apply to local UI bags
/// (messages / query abort already owned by the caller, matching CC's
/// `setMessages` + context clear split).
#[derive(Clone, Debug, Default)]
pub struct ClearConversationOutcome {
    /// Agent IDs whose per-agent caches were preserved across the clear.
    pub preserved_agent_ids: HashSet<String>,
    /// New session id after [`crate::bootstrap::state::regenerate_session_id`].
    pub new_session_id: String,
    /// SessionStart hook messages to append after the clear transcript rows
    /// (CC `setMessages(() => hookMessages)` when non-empty).
    pub session_start_messages: Vec<RenderableMessage>,
}

/// Maps to: CC `shouldKillTask` —
/// `'isBackgrounded' in task && task.isBackgrounded === false`.
pub fn should_kill_task(task: &TaskState) -> bool {
    match task {
        TaskState::LocalShell(task) => !task.is_backgrounded,
        TaskState::Other(other) => other.is_backgrounded == Some(false),
        // Neither carries `isBackgrounded`, so CC's `'isBackgrounded' in task`
        // narrowing is false.
        TaskState::InProcessTeammate(_) | TaskState::Dream(_) => false,
    }
}

/// Collect agent IDs that should survive cache wipe (CC preservedAgentIds).
pub fn collect_preserved_agent_ids(tasks: &BTreeMap<String, Arc<TaskState>>) -> HashSet<String> {
    let mut preserved = HashSet::new();
    for task in tasks.values() {
        if should_kill_task(task) {
            continue;
        }
        match task.as_ref() {
            TaskState::InProcessTeammate(teammate) => {
                let agent_id = if teammate.id.is_empty() {
                    teammate.agent_name.clone()
                } else {
                    teammate.id.clone()
                };
                if !agent_id.is_empty() {
                    preserved.insert(agent_id);
                }
            }
            TaskState::Other(other) if other.task_type == "local_agent" => {
                if !other.id.is_empty() {
                    preserved.insert(other.id.clone());
                }
            }
            TaskState::LocalShell(_) | TaskState::Dream(_) | TaskState::Other(_) => {}
        }
    }
    preserved
}

/// Running preserved `local_agent` tasks whose TaskOutput symlink should be
/// re-pointed after session id regeneration.
fn collect_running_local_agent_repoints(
    tasks: &BTreeMap<String, Arc<TaskState>>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for task in tasks.values() {
        if should_kill_task(task) {
            continue;
        }
        if let TaskState::Other(other) = task.as_ref() {
            if other.task_type == "local_agent" && other.status == "running" && !other.id.is_empty()
            {
                out.push((other.id.clone(), other.id.clone()));
            }
        }
    }
    out
}

/// Partition the tasks map on `/clear`: remove foreground-task entries and
/// return them for the caller to kill; preserve everything else (CC
/// setAppState tasks partition, conversation.ts:136-192). Pure state
/// transform — no kill/abort side effects here.
fn partition_tasks_on_clear(
    tasks: &mut BTreeMap<String, Arc<TaskState>>,
) -> Vec<(String, Arc<TaskState>)> {
    let kill_ids: Vec<String> = tasks
        .iter()
        .filter(|(_, task)| should_kill_task(task))
        .map(|(id, _)| id.clone())
        .collect();
    kill_ids
        .into_iter()
        .filter_map(|task_id| tasks.remove(&task_id).map(|task| (task_id, task)))
        .collect()
}

/// Kill the foreground tasks the partition dropped. Maps to: CC
/// `commands/clear/conversation.ts:146-165` — the abort/cleanup block. CC
/// runs it inside the updater, but its aborts are plain side effects that
/// never write state; the Rust local_agent kill mirrors into `AppState.tasks`
/// (a nested `set_state`), so the kills run AFTER the store install instead.
fn kill_partitioned_tasks(killed: Vec<(String, Arc<TaskState>)>) {
    for (task_id, task) in killed {
        match task.as_ref() {
            TaskState::LocalShell(shell) => {
                // CC :148-153 — kill + cleanup the shell command.
                if let Some(command) = &shell.shell_command {
                    command.kill();
                    command.cleanup();
                }
            }
            TaskState::Other(other) if other.task_type == "local_agent" => {
                // CC :155-159 — the updater aborts the controller directly
                // (`'abortController' in task`); it does NOT run
                // killAsyncAgent, and the AppState entry is already gone.
                // Remove the registry entry too, so every later
                // updateTaskState equivalent on this id (the abort catch's
                // killAsyncAgent, the notification CAS at
                // LocalAgentTask.tsx:299-312, any mirror) is a no-op exactly
                // as CC's framework.ts:55-56 makes it on a deleted id — no
                // ghost `<task-notification>` in the post-clear session, no
                // resurrected AppState entry. Backgrounded agents were never
                // partitioned out and stay untouched.
                if let Some(registry_task) =
                    crate::tasks::local_agent_task::remove_local_agent_task_entry(&task_id)
                {
                    registry_task.lock().unwrap().abort_controller.abort();
                }
            }
            TaskState::InProcessTeammate(_) | TaskState::Dream(_) | TaskState::Other(_) => {}
        }
        // CC :165 — `void evictTaskOutput(taskId)`.
        let _ = futures::executor::block_on(crate::utils::task::disk_output::evict_task_output(
            &task_id,
        ));
    }
}

/// Apply the AppState slice of CC `clearConversation` `setAppState`.
pub fn apply_clear_conversation_app_state(store: &AppStore) {
    // CC conversation.ts:136-192 — the updater only partitions/builds next
    // state; the kills run after the install (see kill_partitioned_tasks).
    let killed = store.replace_with(apply_clear_conversation_app_state_mut);
    kill_partitioned_tasks(killed);
}

fn apply_clear_conversation_app_state_mut(state: &mut AppState) -> Vec<(String, Arc<TaskState>)> {
    // P4 identity: fresh map Arc on the /clear partition (CC's tasks spread).
    let killed = partition_tasks_on_clear(Arc::make_mut(&mut state.tasks));
    state.standalone_agent_context = None;
    state.foregrounded_task_id = None;
    // P4 identity: fresh Arcs mirror CC's fresh-object resets (`{...empty}`).
    state.inbox = Arc::new(crate::hooks::use_inbox_poller::InboxState::default());
    state.worker_sandbox_permissions =
        Arc::new(crate::hooks::use_inbox_poller::WorkerSandboxPermissionsState::default());
    state.pending_worker_request = None;
    state.pending_sandbox_request = None;
    state.file_history = Arc::new(crate::utils::file_history::FileHistoryState::default());
    state.attribution =
        Arc::new(crate::utils::commit_attribution::create_empty_attribution_state());
    state.mcp = Arc::new(crate::state::app_state_store::McpState {
        plugin_reconnect_key: state.mcp.plugin_reconnect_key,
        ..Default::default()
    });
    killed
}

/// Maps to: CC `clearConversation(...)`.
pub fn clear_conversation(store: &AppStore) -> ClearConversationOutcome {
    let snapshot = store.get();
    let preserved_agent_ids = collect_preserved_agent_ids(&snapshot.tasks);
    let local_agent_repoints = collect_running_local_agent_repoints(&snapshot.tasks);
    let worktree_before = crate::utils::worktree::get_current_worktree_session();
    let mode_before = crate::utils::session_storage::get_current_session_metadata().mode;

    // CC `conversation.ts:69-74` passes `getAppState` into
    // `executeSessionEndHooks`, which forwards it to `executeHooksOutsideREPL`
    // (`utils/hooks.ts:4119-4125`); that function merges the session hooks for
    // the MAIN session only — `const sessionId = getSessionId()` (`:3040`),
    // hardcoded, with the comment "Use main session ID for outside-REPL hooks".
    // So unlike the Stop/Task sites there is no `agentId ??` arm here. The read
    // happens before `regenerate_session_id`, so it is still the session being
    // cleared. Gate mirrors CC `:1516` + `:1541`.
    let hooks_config = crate::services::hooks::load_hooks_config_with_session_hooks(
        &crate::bootstrap::state::get_session_id(),
    );
    let _ = futures::executor::block_on(
        crate::services::hooks::lifecycle::execute_session_end_hooks(
            &hooks_config,
            "clear",
            Vec::new(),
        ),
    );

    apply_clear_conversation_app_state(store);

    crate::commands::clear::clear_session_caches(&preserved_agent_ids);
    crate::utils::plans::clear_all_plan_slugs();

    let original = crate::bootstrap::state::get_original_cwd();
    let _ = std::env::set_current_dir(&original);

    crate::utils::session_storage::clear_session_metadata();

    let new_session_id = crate::bootstrap::state::regenerate_session_id(true);
    // Update the environment variable so subprocesses use the new session ID.
    // Maps to: CC `commands/clear/conversation.ts:204-207` — gated on
    // `USER_TYPE === 'ant'` (≙ internal Api capability, the established
    // mapping) and on the variable already being present in the spawn env.
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        let mut update = crate::utils::process_env::begin_update();
        if update
            .snapshot()
            .var_os("CLAUDE_CODE_SESSION_ID")
            .is_some_and(|value| !value.is_empty())
        {
            update.set("CLAUDE_CODE_SESSION_ID", &new_session_id);
        }
        update.commit();
    }
    crate::utils::session_storage::reset_session_file_pointer();

    for (task_id, agent_id) in &local_agent_repoints {
        let target = crate::utils::session_storage::get_agent_transcript_path(agent_id);
        let _ = crate::utils::task::disk_output::init_task_output_as_symlink(task_id, target);
    }

    if let Some(mode) = mode_before {
        crate::utils::session_storage::save_mode(&mode);
    }
    // CC: getCurrentWorktreeSession() then saveWorktreeState — process session
    // survives clearSessionMetadata; re-stamp for --resume.
    if let Some(ref session) = worktree_before {
        crate::utils::session_storage::save_worktree_state(
            crate::utils::worktree::worktree_session_to_persisted_json(session),
        );
    }

    // CC processSessionStartHooks('clear') → setMessages when non-empty.
    let hook_messages = crate::utils::session_start::process_session_start_hooks(
        "clear",
        Some(&new_session_id),
        None,
        None,
    );
    let (_, session_start_messages) =
        crate::utils::session_start::project_hook_result_messages(&hook_messages);

    ClearConversationOutcome {
        preserved_agent_ids,
        new_session_id,
        session_start_messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::spinner::teammate_tree::TeammateTaskSnapshot;
    use crate::hooks::use_swarm_permission_poller::{
        PermissionResponseCallback, TEST_PENDING_CALLBACKS_LOCK, clear_pending_callbacks_for_test,
        has_permission_callback, register_permission_callback,
    };
    use crate::state::app_state_store::TaskStateOther;
    use crate::utils::session_restore::RestoredStandaloneAgentContext;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn collect_preserved_agent_ids_keeps_teammates_and_local_agents() {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            "t1".to_string(),
            Arc::new(TaskState::InProcessTeammate(TeammateTaskSnapshot {
                id: "agent-1".into(),
                agent_name: "worker".into(),
                ..TeammateTaskSnapshot::default()
            })),
        );
        tasks.insert(
            "t2".to_string(),
            Arc::new(TaskState::Other(TaskStateOther {
                id: "agent-2".into(),
                task_type: "local_agent".into(),
                status: "running".into(),
                description: "bg".into(),
                is_backgrounded: Some(true),
                notified: false,
                retain: None,
                evict_after: None,
                progress_tool_uses: None,
                progress_tokens: None,
            })),
        );
        tasks.insert(
            "t3".to_string(),
            Arc::new(TaskState::Other(TaskStateOther {
                id: "shell-1".into(),
                task_type: "local_bash".into(),
                status: "running".into(),
                description: "fg".into(),
                is_backgrounded: Some(false),
                notified: false,
                retain: None,
                evict_after: None,
                progress_tool_uses: None,
                progress_tokens: None,
            })),
        );
        let preserved = collect_preserved_agent_ids(&tasks);
        assert!(preserved.contains("agent-1"));
        assert!(preserved.contains("agent-2"));
        assert!(!preserved.contains("shell-1"));
        assert!(should_kill_task(tasks.get("t3").unwrap()));
        assert!(!should_kill_task(tasks.get("t2").unwrap()));
    }

    #[test]
    fn clear_conversation_kills_foreground_tasks() {
        // clear_conversation regenerates the process-global session id;
        // serialise sibling tests so their id assertions don't race.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| {
            let tasks = Arc::make_mut(&mut state.tasks);
            tasks.insert(
                "fg".to_string(),
                Arc::new(TaskState::Other(TaskStateOther {
                    id: "fg".into(),
                    task_type: "local_bash".into(),
                    status: "running".into(),
                    description: "fg shell".into(),
                    is_backgrounded: Some(false),
                    notified: false,
                    retain: None,
                    evict_after: None,
                    progress_tool_uses: None,
                    progress_tokens: None,
                })),
            );
            tasks.insert(
                "bg".to_string(),
                Arc::new(TaskState::Other(TaskStateOther {
                    id: "bg".into(),
                    task_type: "local_agent".into(),
                    status: "running".into(),
                    description: "bg agent".into(),
                    is_backgrounded: Some(true),
                    notified: false,
                    retain: None,
                    evict_after: None,
                    progress_tool_uses: None,
                    progress_tokens: None,
                })),
            );
        });
        let _ = clear_conversation(&store);
        let tasks = &store.get().tasks;
        assert!(!tasks.contains_key("fg"));
        assert!(tasks.contains_key("bg"));
    }

    #[test]
    fn clear_kills_foreground_agent_without_ghost_notification_or_registry_entry() {
        // CC conversation.ts:146-165 aborts inside the updater and DROPS the
        // entry; afterwards every updateTaskState on the id is a no-op
        // (framework.ts:55-56), so the abort catch's killAsyncAgent and the
        // notification CAS (LocalAgentTask.tsx:299-312) cannot enqueue a
        // ghost `<task-notification>` nor resurrect the entry.
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        // apply_clear_conversation_app_state prunes the process-global
        // permission-callback registry; hold its lock so sibling tests'
        // registrations survive.
        let _callbacks_lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = AppStore::new(AppState::default(), None);
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        let foreground = format!("agent-{}", uuid::Uuid::new_v4());
        let registration = crate::tasks::local_agent_task::register_agent_foreground_with_store(
            crate::tasks::local_agent_task::RegisterAgentForegroundParams {
                agent_id: foreground.clone(),
                description: "fg work".to_string(),
                prompt: "fg".to_string(),
                selected_agent: agent.clone(),
                auto_background_ms: None,
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        let background = format!("agent-{}", uuid::Uuid::new_v4());
        crate::tasks::local_agent_task::register_async_agent_with_store(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: background.clone(),
                description: "bg work".to_string(),
                prompt: "bg".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        assert!(store.get().tasks.contains_key(&foreground));
        // The controller is shared with the registry entry (clone of the same
        // abort flag), so it observes the /clear abort.
        let foreground_abort = crate::tasks::local_agent_task::get_local_agent_task(&foreground)
            .unwrap()
            .abort_controller;
        drop(registration);

        apply_clear_conversation_app_state(&store);

        // The foreground agent: aborted (CC :155-157), AppState entry gone,
        // registry entry gone.
        assert!(
            foreground_abort.is_aborted(),
            "CC conversation.ts:155-157 — the partition aborts the controller"
        );
        assert!(
            !store.get().tasks.contains_key(&foreground),
            "AppState entry dropped by the partition"
        );
        assert!(
            crate::tasks::local_agent_task::get_local_agent_task(&foreground).is_none(),
            "registry entry removed with the partition"
        );
        // Backgrounded agents are preserved, not killed (CC shouldKillTask).
        assert!(store.get().tasks.contains_key(&background));
        assert_eq!(
            crate::tasks::local_agent_task::get_local_agent_task(&background)
                .unwrap()
                .status,
            "running"
        );

        // Simulate the later abort catch: killAsyncAgent + the notification
        // CAS must both be no-ops on the removed id.
        assert!(!crate::tasks::local_agent_task::kill_async_agent(
            &foreground
        ));
        crate::tasks::local_agent_task::enqueue_agent_notification(
            crate::tasks::local_agent_task::EnqueueAgentNotificationParams {
                task_id: &foreground,
                status: "killed",
                error: None,
                final_message: None,
                usage: None,
                worktree: None,
            },
        );
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            0,
            "no ghost task-notification reaches the post-clear session"
        );
        assert!(
            !store.get().tasks.contains_key(&foreground),
            "no mirror resurrects the entry"
        );

        for id in [&foreground, &background] {
            let _ = crate::utils::task::disk_output::cleanup_task_output(id);
        }
        crate::utils::message_queue_manager::clear_command_queue();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
    }

    #[test]
    fn clear_conversation_regenerates_session_and_preserves_callbacks() {
        // Outermost: session id is process-global state; see TEST_ENV_LOCK's
        // contract. Keeps parallel session-mutating tests (bootstrap::state)
        // from clobbering the id between clear_conversation and the asserts.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_pending_callbacks_for_test();

        let before = crate::bootstrap::state::get_session_id();
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| {
            // CC conversation.ts:181-188 preserves the reload dependency.
            Arc::make_mut(&mut state.mcp).plugin_reconnect_key = 7;
            state.auth_version = 3;
            Arc::make_mut(&mut state.tasks).insert(
                "t1".to_string(),
                Arc::new(TaskState::InProcessTeammate(TeammateTaskSnapshot {
                    id: "agent-keep".into(),
                    agent_name: "worker".into(),
                    ..TeammateTaskSnapshot::default()
                })),
            );
            state.pending_worker_request = Some(Arc::new(
                crate::hooks::use_inbox_poller::PendingWorkerRequest {
                    tool_name: "Bash".into(),
                    tool_use_id: "toolu".into(),
                    description: "x".into(),
                },
            ));
            state.standalone_agent_context = Some(RestoredStandaloneAgentContext {
                name: "renamed".into(),
                color: None,
            });
        });

        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-keep".into(),
            tool_use_id: "toolu".into(),
            on_allow: Arc::new(|_, _, _| {}),
            on_reject: Arc::new(|_| {}),
        });

        let outcome = clear_conversation(&store);
        assert_ne!(outcome.new_session_id, before);
        assert_eq!(
            crate::bootstrap::state::get_session_id(),
            outcome.new_session_id
        );
        assert_eq!(
            crate::bootstrap::state::get_parent_session_id().as_deref(),
            Some(before.as_str())
        );
        assert!(outcome.preserved_agent_ids.contains("agent-keep"));
        assert!(has_permission_callback("perm-keep"));
        assert!(store.get().pending_worker_request.is_none());
        assert!(store.get().standalone_agent_context.is_none());
        assert_eq!(store.get().mcp.plugin_reconnect_key, 7);
        assert_eq!(store.get().auth_version, 3);
        clear_pending_callbacks_for_test();
    }

    #[test]
    fn clear_conversation_resets_attribution_and_repersists_worktree_session() {
        // clear_conversation regenerates the process-global session id;
        // serialise sibling tests so their id assertions don't race.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::worktree::restore_worktree_session(Some(
            crate::utils::worktree::WorktreeSession {
                original_cwd: "/repo".into(),
                worktree_path: "/repo/.claude/worktrees/wt".into(),
                worktree_name: "wt".into(),
                session_id: "sess-old".into(),
                ..crate::utils::worktree::WorktreeSession::default()
            },
        ));
        let store = AppStore::new(AppState::default(), None);
        store.replace_with(|state| {
            Arc::make_mut(&mut state.attribution).prompt_count = 7;
        });
        let _ = clear_conversation(&store);
        assert_eq!(store.get().attribution.prompt_count, 0);
        let meta = crate::utils::session_storage::get_current_session_metadata();
        assert!(meta.worktree_session.is_some());
        assert_eq!(
            meta.worktree_session
                .as_ref()
                .and_then(|v| v.get("worktreePath"))
                .and_then(|v| v.as_str()),
            Some("/repo/.claude/worktrees/wt")
        );
        // Live process session still present (CC getCurrentWorktreeSession).
        assert!(crate::utils::worktree::get_current_worktree_session().is_some());
        crate::utils::worktree::restore_worktree_session(None);
    }

    #[test]
    fn clear_conversation_clears_metadata_and_resets_file_pointer() {
        crate::utils::session_storage::set_current_session_metadata_for_test(
            Some("My Session"),
            Some("bug"),
            Some("Renamed"),
            Some(std::path::PathBuf::from("/tmp/old-session.jsonl")),
        );
        crate::utils::session_storage::save_mode("normal");

        let store = AppStore::new(AppState::default(), None);
        let _ = clear_conversation(&store);

        let meta = crate::utils::session_storage::get_current_session_metadata();
        assert!(meta.custom_title.is_none());
        assert!(meta.tag.is_none());
        assert!(meta.agent_name.is_none());
        assert_eq!(meta.mode.as_deref(), Some("normal"));
        assert!(crate::utils::session_storage::current_session_file_for_test().is_none());
    }
}
