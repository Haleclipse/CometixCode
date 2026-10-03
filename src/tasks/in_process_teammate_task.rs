//! In-process teammate task state and AppState-style mutations.
//!
//! Maps to:
//! - CC `tasks/InProcessTeammateTask/types.ts`
//! - CC `tasks/InProcessTeammateTask/InProcessTeammateTask.tsx`
//!
//! This slice ports the official task-state boundary used by teammate/swarm
//! runtime. The `utils/swarm/spawnInProcess.ts` port creates this state; the
//! actual in-process runner remains in the future `utils/swarm/inProcessRunner.ts`
//! slice.

use crate::tool::AbortController;
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::types::message::{AssistantContent, Message, UserContent, UserMessage};
use crate::types::permissions::PermissionMode;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// Maps to CC `tasks/InProcessTeammateTask/types.ts#TeammateIdentity`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeammateIdentity {
    pub agent_id: String,
    pub agent_name: String,
    pub team_name: String,
    pub color: Option<String>,
    pub plan_mode_required: bool,
    pub parent_session_id: String,
}

/// Maps to CC `tasks/InProcessTeammateTask/types.ts#TEAMMATE_MESSAGES_UI_CAP`.
pub const TEAMMATE_MESSAGES_UI_CAP: usize = 50;

/// Maps to CC `tasks/InProcessTeammateTask/types.ts#InProcessTeammateTaskState`.
#[derive(Clone, Debug, PartialEq)]
pub struct InProcessTeammateTaskState {
    pub task_id: String,
    pub task_type: String,
    pub status: String,
    pub description: String,
    pub tool_use_id: Option<String>,
    pub start_time_ms: u64,
    pub end_time_ms: Option<u64>,
    /// Maps to: CC `Task.ts:53` `TaskStateBase.totalPausedMs?: number` — time the
    /// teammate spent parked on a permission prompt, which every elapsed-time
    /// reader subtracts (`TeammateSpinnerLine.tsx:143`,
    /// `InProcessTeammateDetailDialog.tsx:39`, `CoordinatorAgentStatus.tsx:177`,
    /// `AsyncAgentDetailDialog.tsx:41`). Written only by
    /// [`record_teammate_permission_wait_ms`].
    pub total_paused_ms: Option<u64>,
    pub output_file: String,
    pub output_offset: u64,
    pub notified: bool,

    pub identity: TeammateIdentity,
    pub prompt: String,
    pub model: Option<String>,
    pub selected_agent: Option<AgentDefinition>,
    pub abort_controller: Option<AbortController>,
    pub current_work_abort_controller: Option<AbortController>,
    pub awaiting_plan_approval: bool,
    pub permission_mode: PermissionMode,
    pub error: Option<String>,
    pub result: Option<crate::tools::agent_tool::AgentOutput>,
    pub progress: Option<crate::tasks::local_agent_task::AgentProgress>,
    pub messages: Vec<Message>,
    pub in_progress_tool_use_ids: std::collections::HashSet<String>,
    pub pending_user_messages: Vec<String>,
    pub spinner_verb: Option<String>,
    pub past_tense_verb: Option<String>,
    pub is_idle: bool,
    pub shutdown_requested: bool,
    pub last_reported_tool_count: usize,
    pub last_reported_token_count: u64,
}

/// Minimal registration shape used by the `spawnInProcessTeammate` port.
/// Maps to CC `utils/swarm/spawnInProcess.ts` task-state construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterInProcessTeammateParams {
    pub task_id: String,
    pub identity: TeammateIdentity,
    pub description: String,
    pub prompt: String,
    pub selected_agent: Option<AgentDefinition>,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub tool_use_id: Option<String>,
}

static IN_PROCESS_TEAMMATE_TASKS: LazyLock<
    Mutex<HashMap<String, Arc<Mutex<InProcessTeammateTaskState>>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
pub static TEST_IN_PROCESS_TEAMMATE_TASK_LOCK: LazyLock<TestStateLock> =
    LazyLock::new(TestStateLock::new);

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Project registry state into the AppState spinner/UI snapshot.
/// Maps to: CC keeping full TaskState in AppState; Cometix mirrors the UI
/// subset until abort controllers can live in AppState.
pub fn teammate_task_snapshot_from(
    state: &InProcessTeammateTaskState,
) -> crate::components::spinner::teammate_tree::TeammateTaskSnapshot {
    use crate::components::spinner::teammate_tree::{
        TeammateMessageBlockSnapshot, TeammateMessageSnapshot, TeammateTaskSnapshot,
    };
    let messages = state
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => {
                let blocks = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Text(text) => {
                            Some(TeammateMessageBlockSnapshot::Text { text: text.clone() })
                        }
                        AssistantContent::ToolUse(tool_use) => {
                            Some(TeammateMessageBlockSnapshot::ToolUse {
                                name: tool_use.name.clone(),
                                description: None,
                                prompt: None,
                                command: None,
                                query: None,
                                pattern: None,
                            })
                        }
                        _ => Some(TeammateMessageBlockSnapshot::Other),
                    })
                    .collect::<Vec<_>>();
                Some(TeammateMessageSnapshot {
                    message_type: "assistant".to_string(),
                    blocks,
                })
            }
            Message::User(user) => {
                let blocks = user
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        UserContent::Text(text) | UserContent::MetaText(text) => {
                            Some(TeammateMessageBlockSnapshot::Text { text: text.clone() })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                Some(TeammateMessageSnapshot {
                    message_type: "user".to_string(),
                    blocks,
                })
            }
            _ => None,
        })
        .collect();
    // Maps to: CC `TeammateSpinnerLine.tsx:139-146`
    // `Math.max(0, Date.now() - teammate.startTime - (teammate.totalPausedMs ?? 0))`
    // — the frozen work duration excludes time the teammate sat on a permission
    // prompt. `saturating_sub` is the `Math.max(0, ...)`.
    let worked_for_ms = state
        .end_time_ms
        .or_else(|| state.is_idle.then_some(now_ms()))
        .map(|end| {
            end.saturating_sub(state.start_time_ms)
                .saturating_sub(state.total_paused_ms.unwrap_or(0))
        });
    TeammateTaskSnapshot {
        id: state.task_id.clone(),
        task_type: state.task_type.clone(),
        status: state.status.clone(),
        // Maps to: CC `TaskStateBase.notified` — carried into AppState so
        // `evict_terminal_task` applies CC's framework.ts:133 guard.
        notified: state.notified,
        agent_name: state.identity.agent_name.clone(),
        color: state.identity.color.clone(),
        is_idle: state.is_idle,
        idle_text: state.is_idle.then(|| "Idle".to_string()),
        past_tense_status: state.past_tense_verb.clone(),
        shutdown_requested: state.shutdown_requested,
        awaiting_plan_approval: state.awaiting_plan_approval,
        tool_use_count: state.last_reported_tool_count,
        token_count: state.last_reported_token_count as usize,
        worked_for_ms,
        last_activity_description: state.progress.as_ref().and_then(|progress| {
            progress
                .last_activity
                .as_ref()
                .and_then(|activity| activity.activity_description.clone())
        }),
        spinner_verb: state.spinner_verb.clone(),
        recent_activities: Vec::new(),
        messages,
    }
}

fn mirror_teammate_task_to_app_state(task_id: &str) {
    let Some(task) = get_in_process_teammate_task(task_id) else {
        // Rust-only desync (registry entry vanished under its AppState
        // snapshot): thin direct removal; CC cannot express this state.
        crate::utils::task::framework::remove_task_on_bound_store(task_id);
        return;
    };
    let snapshot = teammate_task_snapshot_from(&task);
    crate::utils::task::framework::register_task_on_bound_store(
        crate::state::app_state_store::TaskState::InProcessTeammate(snapshot),
    );
    if is_terminal_task_status(&task.status) {
        // Maps to: CC `utils/swarm/inProcessRunner.ts:1422-1453 / :1476-1506`
        // shape — updateTaskState (terminal transition with notified:true
        // pre-set) followed by `evictTerminalTask(taskId, setAppState)`. The
        // register above is the visibility half; the evict guards
        // (framework.ts:131-140) decide the removal. P4 (2026-08-02):
        // replaced the former unconditional remove.
        crate::utils::task::framework::evict_terminal_task_on_bound_store(task_id);
    }
}

/// Maps to CC `Task.ts#isTerminalTaskStatus`.
pub fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "killed")
}

/// Maps to CC `types.ts#isInProcessTeammateTask`.
pub fn is_in_process_teammate_task(task_type: &str) -> bool {
    task_type == "in_process_teammate"
}

/// Maps to CC `types.ts#appendCappedMessage`.
pub fn append_capped_message<T: Clone>(prev: &[T], item: T) -> Vec<T> {
    if prev.is_empty() {
        return vec![item];
    }
    if prev.len() >= TEAMMATE_MESSAGES_UI_CAP {
        let mut next = prev[prev.len() - (TEAMMATE_MESSAGES_UI_CAP - 1)..].to_vec();
        next.push(item);
        return next;
    }
    let mut next = prev.to_vec();
    next.push(item);
    next
}

/// Maps to CC `utils/swarm/spawnInProcess.ts` registering a teammate task.
pub fn register_in_process_teammate_task(
    params: RegisterInProcessTeammateParams,
) -> InProcessTeammateTaskState {
    let state = InProcessTeammateTaskState {
        task_id: params.task_id.clone(),
        task_type: "in_process_teammate".to_string(),
        status: "running".to_string(),
        description: params.description,
        tool_use_id: params.tool_use_id,
        start_time_ms: now_ms(),
        end_time_ms: None,
        // CC `createTaskStateBase` (`Task.ts:108-125`) leaves `totalPausedMs`
        // undefined; every reader is `?? 0`.
        total_paused_ms: None,
        output_file: crate::utils::task::disk_output::get_task_output_path(&params.task_id)
            .display()
            .to_string(),
        output_offset: 0,
        notified: false,
        identity: params.identity,
        prompt: params.prompt,
        model: params.model,
        selected_agent: params.selected_agent,
        abort_controller: Some(AbortController::default()),
        current_work_abort_controller: None,
        awaiting_plan_approval: false,
        permission_mode: params.permission_mode,
        error: None,
        result: None,
        progress: None,
        messages: Vec::new(),
        in_progress_tool_use_ids: Default::default(),
        pending_user_messages: Vec::new(),
        spinner_verb: None,
        past_tense_verb: None,
        is_idle: false,
        shutdown_requested: false,
        last_reported_tool_count: 0,
        last_reported_token_count: 0,
    };
    IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .insert(state.task_id.clone(), Arc::new(Mutex::new(state.clone())));
    // Maps to: CC `registerTask(taskState, setAppState)`.
    crate::utils::task::framework::register_task_on_bound_store(
        crate::state::app_state_store::TaskState::InProcessTeammate(teammate_task_snapshot_from(
            &state,
        )),
    );
    state
}

/// Maps to CC `InProcessTeammateTask.tsx#requestTeammateShutdown`.
pub fn request_teammate_shutdown(task_id: &str) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if task.status != "running" || task.shutdown_requested {
        return false;
    }
    task.shutdown_requested = true;
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` compaction branch
/// `updateTaskState(taskId, task => ({ ...task, messages: [...] }))`.
pub fn replace_teammate_messages(task_id: &str, messages: Vec<Message>) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if task.status != "running" {
        return false;
    }
    task.messages = messages;
    true
}

/// Maps to CC `InProcessTeammateTask.tsx#appendTeammateMessage`.
pub fn append_teammate_message(task_id: &str, message: Message) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if task.status != "running" {
        return false;
    }
    task.messages = append_capped_message(&task.messages, message);
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` per-turn running update.
pub fn mark_teammate_running(task_id: &str, current_work: Option<AbortController>) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    task.status = "running".to_string();
    task.is_idle = false;
    task.current_work_abort_controller = current_work;
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` clearing
/// `currentWorkAbortController` after an iteration.
pub fn clear_teammate_current_work(task_id: &str) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    task.current_work_abort_controller = None;
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` per-message progress mirror
/// inside the `runAgent(...)` loop (`updateProgressFromMessage`,
/// `getProgressUpdate`, and `inProgressToolUseIDs` updates).
pub fn update_teammate_progress_from_message(
    task_id: &str,
    message: &Message,
    progress: crate::tasks::local_agent_task::AgentProgress,
) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    match message {
        Message::Assistant(assistant) => {
            for block in &assistant.content {
                if let AssistantContent::ToolUse(tool_use) = block {
                    task.in_progress_tool_use_ids.insert(tool_use.id.0.clone());
                }
            }
        }
        Message::User(user) => {
            for block in &user.content {
                if let UserContent::ToolResult(tool_result) = block {
                    task.in_progress_tool_use_ids
                        .remove(&tool_result.tool_use_id.0);
                }
            }
        }
        _ => {}
    }
    task.last_reported_tool_count = progress.tool_use_count;
    task.last_reported_token_count = progress.token_count;
    task.progress = Some(progress);
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:1182-1191` — the
/// `onPermissionWaitMs` callback `createInProcessCanUseTool` is constructed with:
///
/// ```ts
/// (waitMs: number) => {
///   updateTaskState(taskId, task => ({
///     ...task,
///     totalPausedMs: (task.totalPausedMs ?? 0) + waitMs,
///   }), setAppState)
/// }
/// ```
///
/// Accumulates (never replaces): a teammate turn can park on several prompts,
/// and CC's `?? 0` makes the first write identical to starting from zero.
/// `saturating_add` stands for JS number addition, which cannot overflow here.
///
/// Terminal tasks are not written: `updateTaskState` targets a live
/// `in_process_teammate` entry, and the reporter only fires while the runner
/// still owns the turn.
pub fn record_teammate_permission_wait_ms(task_id: &str, wait_ms: u64) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    task.total_paused_ms = Some(task.total_paused_ms.unwrap_or(0).saturating_add(wait_ms));
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` idle transition after a prompt.
pub fn mark_teammate_idle(task_id: &str) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    task.status = "running".to_string();
    task.is_idle = true;
    task.current_work_abort_controller = None;
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` failure path for teammate task state.
pub fn fail_in_process_teammate_task(task_id: &str, error: impl Into<String>) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    task.status = "failed".to_string();
    // Maps to: CC `utils/swarm/inProcessRunner.ts:1489` — the failure
    // transition pre-sets `notified: true` (no XML notification; the SDK
    // bookend is emitted directly), which lets evictTerminalTask pass its
    // framework.ts:133 guard. P4 (2026-08-02).
    task.notified = true;
    task.error = Some(error.into());
    task.end_time_ms = Some(now_ms());
    task.current_work_abort_controller = None;
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to CC `InProcessTeammateTask.tsx#injectUserMessageToTeammate`.
pub fn inject_user_message_to_teammate(task_id: &str, message: impl Into<String>) -> bool {
    let message = message.into();
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    task.pending_user_messages.push(message.clone());
    let user_message = Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(message)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    });
    task.messages = append_capped_message(&task.messages, user_message);
    true
}

/// Maps to CC `utils/swarm/inProcessRunner.ts#waitForNextPromptOrShutdown`
/// popping `pendingUserMessages[0]` from `InProcessTeammateTaskState`.
pub fn pop_pending_user_message_from_teammate(task_id: &str) -> Option<String> {
    let task = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()?;
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) || task.pending_user_messages.is_empty() {
        return None;
    }
    Some(task.pending_user_messages.remove(0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InProcessTeammateTaskIdentity {
    pub status: String,
    pub description: String,
    pub task_type: String,
}

/// Lightweight TaskOutput polling projection that avoids cloning teammate
/// message history every 100ms. Maps to CC reading the same TaskState object in
/// `TaskOutputTool.tsx:135-164`.
pub fn task_identity_snapshot(task_id: &str) -> Option<InProcessTeammateTaskIdentity> {
    let task = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()?;
    let task = task.lock().unwrap();
    Some(InProcessTeammateTaskIdentity {
        status: task.status.clone(),
        description: task.description.clone(),
        task_type: task.task_type.clone(),
    })
}

/// Maps to CC `InProcessTeammateTask.tsx#findTeammateTaskByAgentId`.
pub fn find_teammate_task_by_agent_id(agent_id: &str) -> Option<InProcessTeammateTaskState> {
    let tasks = IN_PROCESS_TEAMMATE_TASKS.lock().unwrap();
    let mut fallback = None;
    for task in tasks.values() {
        let task = task.lock().unwrap();
        if task.identity.agent_id == agent_id {
            if task.status == "running" {
                return Some(task.clone());
            }
            fallback.get_or_insert_with(|| task.clone());
        }
    }
    fallback
}

/// Maps to CC `InProcessTeammateTask.tsx#getAllInProcessTeammateTasks`.
pub fn get_all_in_process_teammate_tasks() -> Vec<InProcessTeammateTaskState> {
    IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .values()
        .map(|task| task.lock().unwrap().clone())
        .collect()
}

/// Maps to CC `InProcessTeammateTask.tsx#getRunningTeammatesSorted`.
pub fn get_running_teammates_sorted() -> Vec<InProcessTeammateTaskState> {
    let mut tasks = get_all_in_process_teammate_tasks()
        .into_iter()
        .filter(|task| task.status == "running")
        .collect::<Vec<_>>();
    tasks.sort_by(|a, b| a.identity.agent_name.cmp(&b.identity.agent_name));
    tasks
}

/// Maps to CC `killInProcessTeammate(...)` task-state termination side effect;
/// full runner abort lives in the future `utils/swarm/spawnInProcess.ts` port.
pub fn kill_in_process_teammate(task_id: &str) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if is_terminal_task_status(&task.status) {
        return false;
    }
    if let Some(abort) = &task.abort_controller {
        abort.abort();
    }
    if let Some(abort) = &task.current_work_abort_controller {
        abort.abort();
    }
    task.status = "killed".to_string();
    // Maps to: CC `utils/swarm/spawnInProcess.ts:284-286` — the kill
    // transition writes `status: 'killed', notified: true, endTime`, so the
    // eager eviction path (evictTerminalTask, framework.ts:133) passes its
    // notified guard. P4 (2026-08-02).
    task.notified = true;
    task.end_time_ms = Some(now_ms());
    drop(task);
    mirror_teammate_task_to_app_state(task_id);
    true
}

/// Maps to: CC `useInboxPoller.ts:749-765` — on a leader-side shutdown
/// approval, mark the teammate's in-process task(s) completed so
/// `hasRunningTeammates` flips false and the spinner stops. CC writes only
/// `status: 'completed'` + `endTime` (no `notified`) for every task with
/// `isInProcessTeammateTask(task) && task.identity.agentId === teammateId`,
/// inside the same setAppState that removes the teammate from teamContext.
/// Rust wiring note: the poll core is by-value with no store access, so the
/// task half runs here and mirrors into `AppState.tasks` via the bound store
/// (register only — CC's :753-765 write does not evict; the un-notified
/// snapshot stays in the map exactly like CC's completed-but-not-notified
/// task). Membership comes from the bound store's AppState.tasks (CC
/// :753-754 iterates `{...prev.tasks}` entries); the registry only supplies
/// the snapshot data. P4 (2026-08-02).
pub fn complete_in_process_teammate_tasks_for_agent(agent_id: &str) -> usize {
    // CC useInboxPoller.ts:753-754 — `const updatedTasks = {...prev.tasks}`
    // + `Object.entries(updatedTasks)`: the write iterates only entries that
    // currently exist in AppState.tasks. Membership therefore comes from the
    // bound store's AppState.tasks; the never-pruned registry is only the
    // snapshot data source (agent identity + full state). A task already
    // evicted from AppState.tasks must NOT be resurrected here. P4 review
    // fix (2026-08-02).
    let Some(app_tasks) =
        crate::utils::task::framework::with_bound_task_app_store(|store| store.get().tasks.clone())
    else {
        return 0;
    };
    let mut completed = 0usize;
    for (task_id, app_task) in app_tasks.iter() {
        // CC :756-757 — `isInProcessTeammateTask(task)`.
        if app_task.as_in_process_teammate().is_none() {
            continue;
        }
        // Registry lookup for the full state (identity + snapshot fields);
        // an AppState entry without a registry backer is a Rust-only desync
        // CC cannot express — skip rather than fabricate.
        let Some(task) = IN_PROCESS_TEAMMATE_TASKS
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
        else {
            continue;
        };
        let snapshot = {
            let mut task = task.lock().unwrap();
            // CC :757 — `task.identity.agentId === teammateId`.
            if task.identity.agent_id != agent_id {
                continue;
            }
            // CC :759-763 — unconditional overwrite to 'completed' + endTime
            // (no terminal guard on this write point).
            task.status = "completed".to_string();
            task.end_time_ms = Some(now_ms());
            teammate_task_snapshot_from(&task)
        };
        // CC :759 `updatedTasks[tid] = {...task, status, endTime}` — fresh
        // per-task reference installed into a fresh tasks map.
        crate::utils::task::framework::register_task_on_bound_store(
            crate::state::app_state_store::TaskState::InProcessTeammate(snapshot),
        );
        completed += 1;
    }
    completed
}

/// Atomically mark a teammate task notified after TaskOutput retrieves a
/// terminal result. Maps to CC `TaskOutputTool.tsx:259-263,314-318`.
pub fn mark_in_process_teammate_notified(task_id: &str) -> bool {
    let Some(task) = IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return false;
    };
    let mut task = task.lock().unwrap();
    if task.notified {
        return false;
    }
    task.notified = true;
    true
}

pub fn get_in_process_teammate_task(task_id: &str) -> Option<InProcessTeammateTaskState> {
    IN_PROCESS_TEAMMATE_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
        .map(|task| task.lock().unwrap().clone())
}

#[cfg(test)]
pub fn clear_in_process_teammate_tasks_for_test() {
    IN_PROCESS_TEAMMATE_TASKS.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> TeammateIdentity {
        TeammateIdentity {
            agent_id: format!("{name}@team"),
            agent_name: name.to_string(),
            team_name: "team".to_string(),
            color: Some("blue".to_string()),
            plan_mode_required: false,
            parent_session_id: "session-parent".to_string(),
        }
    }

    fn register(name: &str, status: Option<&str>) -> String {
        let task_id = format!("task-{name}-{}", uuid::Uuid::new_v4());
        register_in_process_teammate_task(RegisterInProcessTeammateParams {
            task_id: task_id.clone(),
            identity: identity(name),
            description: format!("{name} teammate"),
            prompt: "help".to_string(),
            selected_agent: None,
            model: None,
            permission_mode: PermissionMode::Default,
            tool_use_id: None,
        });
        if let Some(status) = status {
            let task = IN_PROCESS_TEAMMATE_TASKS
                .lock()
                .unwrap()
                .get(&task_id)
                .cloned()
                .unwrap();
            task.lock().unwrap().status = status.to_string();
        }
        task_id
    }

    #[test]
    fn shutdown_completion_does_not_resurrect_evicted_tasks() {
        // CC useInboxPoller.ts:753-754 — the completed overwrite iterates
        // `{...prev.tasks}` entries only; a task no longer in AppState.tasks
        // must not be re-registered by the shutdown-approval path.
        let _lock = TEST_IN_PROCESS_TEAMMATE_TASK_LOCK.lock().unwrap();
        clear_in_process_teammate_tasks_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        crate::utils::task::framework::bind_task_app_store(Some(store.clone()));

        let evicted_task = register("carol", None);
        let live_task = register("carol", None);
        assert!(store.get().tasks.contains_key(&evicted_task));
        assert!(store.get().tasks.contains_key(&live_task));
        // Simulate a prior eviction of the first task from AppState.tasks
        // (the registry keeps its entry — it is never pruned).
        crate::utils::task::framework::remove_task_on_bound_store(&evicted_task);
        assert!(!store.get().tasks.contains_key(&evicted_task));

        let completed = complete_in_process_teammate_tasks_for_agent("carol@team");
        assert_eq!(completed, 1, "only the AppState-present entry is rewritten");
        // The evicted task stays evicted.
        assert!(!store.get().tasks.contains_key(&evicted_task));
        // The live entry is overwritten to completed in both stores.
        let live = store.get().tasks.get(&live_task).cloned().unwrap();
        assert_eq!(live.as_in_process_teammate().unwrap().status, "completed");
        assert_eq!(
            get_in_process_teammate_task(&live_task).unwrap().status,
            "completed"
        );
        // The evicted task's registry entry is untouched (membership gate ran
        // before the overwrite).
        assert_eq!(
            get_in_process_teammate_task(&evicted_task).unwrap().status,
            "running"
        );

        crate::utils::task::framework::bind_task_app_store(None);
    }

    /// Maps to: CC `inProcessRunner.ts:1182-1191` (`totalPausedMs:
    /// (task.totalPausedMs ?? 0) + waitMs`) and `TeammateSpinnerLine.tsx:143`
    /// (`Date.now() - teammate.startTime - (teammate.totalPausedMs ?? 0)`).
    #[test]
    fn permission_wait_accumulates_and_is_subtracted_from_worked_duration() {
        let _lock = TEST_IN_PROCESS_TEAMMATE_TASK_LOCK.lock().unwrap();
        clear_in_process_teammate_tasks_for_test();
        let task_id = register("paused", None);
        assert!(
            get_in_process_teammate_task(&task_id)
                .unwrap()
                .total_paused_ms
                .is_none()
        );

        // Two prompts in one turn accumulate; CC's `?? 0` makes the first write
        // identical to starting from zero.
        assert!(record_teammate_permission_wait_ms(&task_id, 1_200));
        assert!(record_teammate_permission_wait_ms(&task_id, 800));
        let state = get_in_process_teammate_task(&task_id).unwrap();
        assert_eq!(state.total_paused_ms, Some(2_000));

        // The frozen work duration a finished teammate reports excludes it.
        let mut ended = state.clone();
        ended.end_time_ms = Some(ended.start_time_ms + 5_000);
        assert_eq!(
            teammate_task_snapshot_from(&ended).worked_for_ms,
            Some(3_000)
        );
        // `Math.max(0, ...)`: a longer pause than the whole span floors at 0.
        ended.total_paused_ms = Some(9_000);
        assert_eq!(teammate_task_snapshot_from(&ended).worked_for_ms, Some(0));

        // `updateTaskState` targets a live entry; a terminal task is not written.
        assert!(fail_in_process_teammate_task(&task_id, "boom"));
        assert!(!record_teammate_permission_wait_ms(&task_id, 500));
        assert_eq!(
            get_in_process_teammate_task(&task_id)
                .unwrap()
                .total_paused_ms,
            Some(2_000)
        );
    }

    #[test]
    fn append_capped_message_keeps_last_50_like_official_ui_cap() {
        let messages = (0..TEAMMATE_MESSAGES_UI_CAP)
            .map(|idx| idx.to_string())
            .collect::<Vec<_>>();
        let next = append_capped_message(&messages, "new".to_string());
        assert_eq!(next.len(), TEAMMATE_MESSAGES_UI_CAP);
        assert_eq!(next.first().map(String::as_str), Some("1"));
        assert_eq!(next.last().map(String::as_str), Some("new"));
    }

    #[test]
    fn teammate_progress_updates_track_tool_use_ids_like_official_runner() {
        let _lock = TEST_IN_PROCESS_TEAMMATE_TASK_LOCK.lock().unwrap();
        clear_in_process_teammate_tasks_for_test();
        let task_id = register("progress", None);
        let assistant_tool_use = Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_1".to_string()),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path":"/tmp/a"}),
                },
            )],
            model: None,
            stop_reason: None,
            usage: Some(crate::types::message::TokenUsage {
                input_tokens: 10,
                output_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 2,
                cache_deleted_input_tokens: 0,
            }),
        });
        let mut tracker = crate::tasks::local_agent_task::create_progress_tracker();
        crate::tasks::local_agent_task::update_progress_from_message(
            &mut tracker,
            &assistant_tool_use,
        );
        let progress = crate::tasks::local_agent_task::get_progress_update(&tracker);
        assert!(update_teammate_progress_from_message(
            &task_id,
            &assistant_tool_use,
            progress
        ));
        let state = get_in_process_teammate_task(&task_id).unwrap();
        assert_eq!(state.progress.as_ref().unwrap().tool_use_count, 1);
        assert_eq!(state.progress.as_ref().unwrap().token_count, 15);
        assert!(state.in_progress_tool_use_ids.contains("toolu_1"));

        let tool_result = Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::ToolResult(
                crate::types::message::ToolResult {
                    tool_use_id: crate::types::ids::ToolUseId("toolu_1".to_string()),
                    content: "ok".to_string(),
                    is_error: false,
                    content_blocks: Vec::new(),
                    tool_use_result: None,
                },
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let progress = crate::tasks::local_agent_task::get_progress_update(&tracker);
        assert!(update_teammate_progress_from_message(
            &task_id,
            &tool_result,
            progress
        ));
        assert!(
            get_in_process_teammate_task(&task_id)
                .unwrap()
                .in_progress_tool_use_ids
                .is_empty()
        );
    }

    #[test]
    fn teammate_message_injection_shutdown_and_lookup_follow_official_state_rules() {
        let _lock = TEST_IN_PROCESS_TEAMMATE_TASK_LOCK.lock().unwrap();
        clear_in_process_teammate_tasks_for_test();
        let alice_task = register("alice", None);
        let _old_alice = register("alice", Some("killed"));
        let bob_task = register("bob", None);

        assert!(request_teammate_shutdown(&alice_task));
        assert!(!request_teammate_shutdown(&alice_task));
        assert!(
            get_in_process_teammate_task(&alice_task)
                .unwrap()
                .shutdown_requested
        );

        assert!(inject_user_message_to_teammate(&alice_task, "hello alice"));
        let alice = get_in_process_teammate_task(&alice_task).unwrap();
        assert_eq!(alice.pending_user_messages, vec!["hello alice".to_string()]);
        assert_eq!(alice.messages.len(), 1);
        assert_eq!(
            pop_pending_user_message_from_teammate(&alice_task).as_deref(),
            Some("hello alice")
        );
        assert!(
            get_in_process_teammate_task(&alice_task)
                .unwrap()
                .pending_user_messages
                .is_empty()
        );
        assert_eq!(
            find_teammate_task_by_agent_id("alice@team")
                .unwrap()
                .task_id,
            alice_task
        );

        kill_in_process_teammate(&alice_task);
        assert!(!inject_user_message_to_teammate(&alice_task, "too late"));
        assert_eq!(
            find_teammate_task_by_agent_id("alice@team")
                .unwrap()
                .identity
                .agent_id,
            "alice@team"
        );
        let sorted = get_running_teammates_sorted();
        assert_eq!(
            sorted
                .iter()
                .map(|task| task.identity.agent_name.as_str())
                .collect::<Vec<_>>(),
            vec!["bob"]
        );
        assert_eq!(
            get_in_process_teammate_task(&bob_task).unwrap().status,
            "running"
        );
    }
}
