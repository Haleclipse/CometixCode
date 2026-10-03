//! Local background agent task state.
//!
//! Maps to CC `tasks/LocalAgentTask/LocalAgentTask.tsx`.
//!
//! This module owns the local-agent task registry, output-file initialization,
//! progress snapshots, completion/failure transitions, and task-notification XML
//! assembly. The rich state (abort handles, messages, progress) lives in the
//! in-memory registry; every lifecycle transition mirrors the UI-visible subset
//! into `AppState.tasks` (`mirror_task_to_app_state`). Until the registry
//! dissolves into `AppState.tasks`, the mirror's actual consumers are: the
//! `/clear` partition (`commands/clear/conversation.rs`), the turn-duration
//! line (`components/messages/turn_duration_message.rs`), the
//! stop/evict lookups (`tasks/stop_task.rs`,
//! `utils/task/framework.rs::evict_terminal_task`), and change notification
//! (PromptInput's `s.tasks` subscription schedules the frame on which the
//! footer pill / ↓ tasks dialog re-read the registry snapshot — the items
//! themselves still come from the registry, not from AppState).

use crate::constants::xml::{
    OUTPUT_FILE_TAG, STATUS_TAG, SUMMARY_TAG, TASK_ID_TAG, TASK_NOTIFICATION_TAG, TOOL_USE_ID_TAG,
    WORKTREE_BRANCH_TAG, WORKTREE_PATH_TAG, WORKTREE_TAG,
};
use crate::tool::AbortController;
use crate::tools::agent_tool::agent_tool_utils::CompletedAgentRun;
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::types::message::{AssistantContent, Message};
use crate::utils::task::disk_output;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::HashMap;
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};

const MAX_RECENT_ACTIVITIES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolActivity {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub activity_description: Option<String>,
    pub is_search: bool,
    pub is_read: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentProgress {
    pub tool_use_count: usize,
    pub token_count: u64,
    pub last_activity: Option<ToolActivity>,
    pub recent_activities: Vec<ToolActivity>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressTracker {
    pub tool_use_count: usize,
    pub latest_input_tokens: u64,
    pub cumulative_output_tokens: u64,
    pub recent_activities: Vec<ToolActivity>,
}

/// Maps to CC `LocalAgentTask.tsx#createProgressTracker`.
pub fn create_progress_tracker() -> ProgressTracker {
    ProgressTracker::default()
}

/// Maps to CC `LocalAgentTask.tsx#getTokenCountFromTracker`.
pub fn get_token_count_from_tracker(tracker: &ProgressTracker) -> u64 {
    tracker.latest_input_tokens + tracker.cumulative_output_tokens
}

/// Maps to CC `LocalAgentTask.tsx#updateProgressFromMessage`.
pub fn update_progress_from_message(tracker: &mut ProgressTracker, message: &Message) {
    let Message::Assistant(assistant) = message else {
        return;
    };
    if let Some(usage) = &assistant.usage {
        tracker.latest_input_tokens =
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
        tracker.cumulative_output_tokens += usage.output_tokens;
    }
    for block in &assistant.content {
        if let AssistantContent::ToolUse(tool_use) = block {
            tracker.tool_use_count += 1;
            if tool_use.name != crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME {
                // Maps to CC `createActivityDescriptionResolver(tools)` and
                // `getToolSearchOrReadInfo(...)`. The Rust registry exposes the
                // same metadata on ToolCall; name fallbacks retain behavior for
                // tools whose metadata has not yet been ported.
                let tool = crate::services::tools::tool_execution::find_tool_call(&tool_use.name);
                let classification =
                    tool.and_then(|tool| tool.is_search_or_read_command(&tool_use.input));
                tracker.recent_activities.push(ToolActivity {
                    tool_name: tool_use.name.clone(),
                    input: tool_use.input.clone(),
                    activity_description: tool
                        .and_then(|tool| tool.get_activity_description(&tool_use.input)),
                    is_search: classification.map_or_else(
                        || matches!(tool_use.name.as_str(), "Glob" | "Grep" | "WebSearch"),
                        |classification| classification.is_search,
                    ),
                    is_read: classification.map_or_else(
                        || matches!(tool_use.name.as_str(), "Read" | "NotebookRead"),
                        |classification| classification.is_read,
                    ),
                });
            }
        }
    }
    while tracker.recent_activities.len() > MAX_RECENT_ACTIVITIES {
        tracker.recent_activities.remove(0);
    }
}

/// Maps to CC `LocalAgentTask.tsx#getProgressUpdate`.
pub fn get_progress_update(tracker: &ProgressTracker) -> AgentProgress {
    AgentProgress {
        tool_use_count: tracker.tool_use_count,
        token_count: get_token_count_from_tracker(tracker),
        last_activity: tracker.recent_activities.last().cloned(),
        recent_activities: tracker.recent_activities.clone(),
        summary: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAgentTaskState {
    pub task_id: String,
    pub task_type: String,
    pub status: String,
    pub agent_id: String,
    pub prompt: String,
    pub selected_agent: Option<AgentDefinition>,
    pub agent_type: String,
    pub description: String,
    pub model: Option<String>,
    pub error: Option<String>,
    pub result: Option<CompletedAgentRun>,
    pub progress: Option<AgentProgress>,
    pub retrieved: bool,
    pub messages: Vec<Message>,
    pub last_reported_tool_count: usize,
    pub last_reported_token_count: u64,
    pub is_backgrounded: bool,
    pub pending_messages: Vec<String>,
    pub retain: bool,
    pub disk_loaded: bool,
    pub start_time_ms: u64,
    pub end_time_ms: Option<u64>,
    pub tool_use_id: Option<String>,
    pub output_file: String,
    pub abort_controller: AbortController,
    /// Maps to CC `TaskStateBase.notified` duplicate-notification guard.
    pub notified: bool,
    /// Maps to CC `LocalAgentTask.tsx:198` `evictAfter` — panel visibility
    /// deadline (epoch ms); `None` = no deadline (running or retained). Set at
    /// the terminal transitions (`:379`, `:526`, `:556`).
    pub evict_after: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalAgentTaskOutputSnapshot {
    pub task_id: String,
    pub task_type: String,
    pub status: String,
    pub description: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub prompt: Option<String>,
    pub result: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterAsyncAgentParams {
    pub agent_id: String,
    pub description: String,
    pub prompt: String,
    pub selected_agent: AgentDefinition,
    pub tool_use_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterAgentForegroundParams {
    pub agent_id: String,
    pub description: String,
    pub prompt: String,
    pub selected_agent: AgentDefinition,
    pub auto_background_ms: Option<u64>,
    pub tool_use_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AutoBackgroundCancel {
    cancelled: Arc<AtomicBool>,
}

impl AutoBackgroundCancel {
    /// Maps to CC `registerAgentForeground(...)` returned `cancelAutoBackground`.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct ForegroundAgentRegistration {
    pub task_id: String,
    pub background_signal: tokio::sync::watch::Receiver<bool>,
    pub cancel_auto_background: Option<AutoBackgroundCancel>,
}

static LOCAL_AGENT_TASKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<LocalAgentTaskState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Root `setAppState` equivalent retained beside non-serializable task state.
static LOCAL_AGENT_TASK_STORES: LazyLock<Mutex<HashMap<String, crate::state::store::AppStore>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static BACKGROUND_SIGNAL_RESOLVERS: LazyLock<
    Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
pub static TEST_LOCAL_AGENT_TASK_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Maps to CC `LocalAgentTask.tsx#isLocalAgentTask`.
pub fn is_local_agent_task(task_type: &str) -> bool {
    task_type == "local_agent"
}

/// Mirror the registry snapshot for `task_id` into `AppState.tasks` as a
/// `TaskState::Other` stub so the footer pill, the ↓ tasks dialog, and the
/// `/clear` partition see local_agent tasks.
///
/// Maps to: CC `tasks/LocalAgentTask/LocalAgentTask.tsx` writing every
/// lifecycle transition into `AppState.tasks` through
/// `registerTask`/`updateTaskState`/`setAppState` (`:627`, `:699`, `:708-717`,
/// `:752-764` and the terminal transitions at `:379`/`:526`/`:556`). Rust
/// keeps the rich state (abort handles, messages, progress) in
/// `LOCAL_AGENT_TASKS` and projects the UI-visible subset; headless paths that
/// captured no store skip the mirror.
///
/// Lock ordering: registry locks are taken and RELEASED before the store
/// write — never call this while holding a task's Mutex guard.
fn mirror_task_to_app_state(task_id: &str) {
    let Some(store) = LOCAL_AGENT_TASK_STORES
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
    else {
        return;
    };
    let Some(task) = get_local_agent_task(task_id) else {
        return;
    };
    crate::utils::task::framework::register_task(
        crate::state::app_state_store::TaskState::Other(
            crate::state::app_state_store::TaskStateOther {
                id: task.task_id.clone(),
                task_type: task.task_type.clone(),
                status: task.status,
                description: task.description,
                is_backgrounded: Some(task.is_backgrounded),
                // CC `LocalAgentTask.tsx:191` declares `retain: boolean`
                // required, so the field is always present on the mirror.
                retain: Some(task.retain),
                notified: task.notified,
                evict_after: task.evict_after,
                // CC `LocalAgentTask.tsx:178` `progress` — the two-field
                // projection the `Other` stub carries.
                progress_tool_uses: task
                    .progress
                    .as_ref()
                    .map(|progress| progress.tool_use_count),
                progress_tokens: task.progress.as_ref().map(|progress| progress.token_count),
            },
        ),
        &store,
    );
}

/// Registry-side `registerTask` for a pre-built local_agent-family state.
/// Main-session tasks reuse `LocalAgentTaskState` (CC
/// `LocalMainSessionTask.ts:54-57`), and the registry statics are
/// module-private — this is the insert seam `local_main_session_task` builds
/// on. Maps to CC `utils/task/framework.ts` `registerTask(taskState,
/// setAppState)` applied to that state.
pub(crate) fn insert_task_state(
    state: LocalAgentTaskState,
    root_store: Option<crate::state::store::AppStore>,
) {
    let task_id = state.task_id.clone();
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .insert(task_id.clone(), Arc::new(Mutex::new(state)));
    if let Some(root_store) = root_store {
        LOCAL_AGENT_TASK_STORES
            .lock()
            .unwrap()
            .insert(task_id.clone(), root_store);
    }
    mirror_task_to_app_state(&task_id);
}

/// Registry-side `updateTaskState` + AppState mirror for local_agent-family
/// entries. Returns `None` when the task is unknown. Maps to CC
/// `utils/task/framework.ts` `updateTaskState` applied to the registry copy.
///
/// CC framework.ts:59-63 — an updater that returns the same reference does
/// NOT notify subscribers. The `&mut` closure cannot express reference
/// identity, so the closest carrier is a value comparison: an update that
/// changed nothing skips the mirror (and with it the store install/notify).
/// Real progress changes still differ in value (the mirror projects
/// `progress_tool_uses`/`progress_tokens`), so they notify as before.
pub(crate) fn update_task_state_and_mirror<R>(
    task_id: &str,
    update: impl FnOnce(&mut LocalAgentTaskState) -> R,
) -> Option<R> {
    let task = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned()?;
    let (result, changed) = {
        let mut task = task.lock().unwrap();
        let before = task.clone();
        let result = update(&mut task);
        (result, *task != before)
    };
    if changed {
        mirror_task_to_app_state(task_id);
    }
    Some(result)
}

/// Maps to: CC `utils/task/framework.ts:87-97` — re-registering an existing
/// task MERGES: the new state wins except `retain`, `startTime`
/// (`start_time_ms`), `messages`, `diskLoaded` (`disk_loaded`) and
/// `pendingMessages` (`pending_messages`), which are carried forward from the
/// existing entry (CC's comment: the user's just-appended prompt lives in
/// `messages` and isn't on disk yet). Registry-side twin of the AppState-side
/// merge in `utils/task/framework.rs::register_task`.
///
/// Consequence for the resume path (`resume_agent.rs` re-registers a live
/// agent id through `register_async_agent_with_store`): elapsed time stays
/// continuous (`start_time_ms` survives the replace, keeping the panel sort
/// stable), and messages queued via SendMessage plus the viewed transcript
/// survive the re-registration instead of resetting.
fn merge_reregistered_task_state(state: &mut LocalAgentTaskState) {
    let existing = LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .get(&state.task_id)
        .cloned();
    let Some(existing) = existing else {
        return;
    };
    let existing = existing.lock().unwrap();
    // CC :91-95 — the five carried-forward fields; everything else (status,
    // description, progress, …) comes from the new state.
    state.retain = existing.retain;
    state.start_time_ms = existing.start_time_ms;
    state.messages = existing.messages.clone();
    state.disk_loaded = existing.disk_loaded;
    state.pending_messages = existing.pending_messages.clone();
}

/// Maps to CC `LocalAgentTask.tsx#registerAsyncAgent`.
pub fn register_async_agent(params: RegisterAsyncAgentParams) -> LocalAgentTaskState {
    register_async_agent_with_store(params, None)
}

pub fn register_async_agent_with_store(
    params: RegisterAsyncAgentParams,
    root_store: Option<crate::state::store::AppStore>,
) -> LocalAgentTaskState {
    let transcript_path =
        crate::utils::session_storage::get_agent_transcript_path(&params.agent_id);
    let output_file = disk_output::init_task_output_as_symlink(&params.agent_id, transcript_path)
        .unwrap_or_else(|_| disk_output::get_task_output_path(&params.agent_id))
        .display()
        .to_string();
    let abort_controller = AbortController::default();
    let mut state = LocalAgentTaskState {
        task_id: params.agent_id.clone(),
        task_type: "local_agent".to_string(),
        status: "running".to_string(),
        agent_id: params.agent_id.clone(),
        prompt: params.prompt,
        agent_type: params.selected_agent.agent_type.clone(),
        description: params.description,
        model: params.selected_agent.model.clone(),
        selected_agent: Some(params.selected_agent),
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
        start_time_ms: now_ms(),
        end_time_ms: None,
        tool_use_id: params.tool_use_id,
        output_file,
        abort_controller,
        notified: false,
        evict_after: None,
    };
    // CC framework.ts:87-97 — the resume path re-registers a live id; carry
    // forward the UI-held/merge-listed fields instead of resetting them.
    merge_reregistered_task_state(&mut state);
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .insert(state.task_id.clone(), Arc::new(Mutex::new(state.clone())));
    if let Some(root_store) = root_store {
        LOCAL_AGENT_TASK_STORES
            .lock()
            .unwrap()
            .insert(state.task_id.clone(), root_store);
    }
    // Maps to CC `LocalAgentTask.tsx:627` `registerTask(taskState, setAppState)`.
    mirror_task_to_app_state(&state.task_id);
    state
}

/// Maps to CC `LocalAgentTask.tsx#registerAgentForeground`.
pub fn register_agent_foreground(
    params: RegisterAgentForegroundParams,
) -> ForegroundAgentRegistration {
    register_agent_foreground_with_store(params, None)
}

pub fn register_agent_foreground_with_store(
    params: RegisterAgentForegroundParams,
    root_store: Option<crate::state::store::AppStore>,
) -> ForegroundAgentRegistration {
    let transcript_path =
        crate::utils::session_storage::get_agent_transcript_path(&params.agent_id);
    let output_file = disk_output::init_task_output_as_symlink(&params.agent_id, transcript_path)
        .unwrap_or_else(|_| disk_output::get_task_output_path(&params.agent_id))
        .display()
        .to_string();
    let abort_controller = AbortController::default();
    let mut state = LocalAgentTaskState {
        task_id: params.agent_id.clone(),
        task_type: "local_agent".to_string(),
        status: "running".to_string(),
        agent_id: params.agent_id.clone(),
        prompt: params.prompt,
        agent_type: params.selected_agent.agent_type.clone(),
        description: params.description,
        model: params.selected_agent.model.clone(),
        selected_agent: Some(params.selected_agent),
        error: None,
        result: None,
        progress: None,
        retrieved: false,
        messages: Vec::new(),
        last_reported_tool_count: 0,
        last_reported_token_count: 0,
        is_backgrounded: false,
        pending_messages: Vec::new(),
        retain: false,
        disk_loaded: false,
        start_time_ms: now_ms(),
        end_time_ms: None,
        tool_use_id: params.tool_use_id,
        output_file,
        abort_controller,
        notified: false,
        evict_after: None,
    };
    // CC framework.ts:87-97 — same re-register merge as the async path.
    merge_reregistered_task_state(&mut state);
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .insert(state.task_id.clone(), Arc::new(Mutex::new(state.clone())));
    if let Some(root_store) = root_store {
        LOCAL_AGENT_TASK_STORES
            .lock()
            .unwrap()
            .insert(state.task_id.clone(), root_store);
    }

    let (sender, background_signal) = tokio::sync::watch::channel(false);
    BACKGROUND_SIGNAL_RESOLVERS
        .lock()
        .unwrap()
        .insert(state.task_id.clone(), sender);

    // Maps to CC `LocalAgentTask.tsx:699` `registerTask(taskState, setAppState)`
    // (after the backgroundSignal promise is created, before auto-background).
    mirror_task_to_app_state(&state.task_id);

    let cancel_auto_background = params.auto_background_ms.and_then(|milliseconds| {
        (milliseconds > 0).then(|| {
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancelled_for_thread = cancelled.clone();
            let task_id = state.task_id.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(milliseconds));
                if !cancelled_for_thread.load(Ordering::SeqCst) {
                    let _ = background_agent_task(&task_id);
                }
            });
            AutoBackgroundCancel { cancelled }
        })
    });

    ForegroundAgentRegistration {
        task_id: state.task_id,
        background_signal,
        cancel_auto_background,
    }
}

/// Maps to CC `LocalAgentTask.tsx:740-774` `backgroundAgentTask`.
pub fn background_agent_task(task_id: &str) -> bool {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return false;
    };
    {
        let mut task = task.lock().unwrap();
        // CC `:747` — `if (!isLocalAgentTask(task) || task.isBackgrounded)
        // return false`: the missing-entry arm above is the guard's first
        // half; there is NO status condition (a terminal foreground task can
        // still be flipped, exactly as in CC).
        if task.is_backgrounded {
            return false;
        }
        task.is_backgrounded = true;
    }
    // Maps to CC `:752-764` setAppState `{...prevTask, isBackgrounded: true}`,
    // applied before the background-signal resolver fires (`:766-771`).
    mirror_task_to_app_state(task_id);
    if let Some(sender) = BACKGROUND_SIGNAL_RESOLVERS.lock().unwrap().remove(task_id) {
        let _ = sender.send(true);
    }
    true
}

/// Maps to CC `LocalAgentTask.tsx#unregisterAgentForeground`.
pub fn unregister_agent_foreground(task_id: &str) -> bool {
    BACKGROUND_SIGNAL_RESOLVERS.lock().unwrap().remove(task_id);
    let task = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned();
    let Some(task) = task else {
        return false;
    };
    if task.lock().unwrap().is_backgrounded {
        return false;
    }
    let removed = LOCAL_AGENT_TASKS.lock().unwrap().remove(task_id).is_some();
    if removed {
        // Maps to CC `:788-799` — the AppState entry is dropped with the
        // registration (`const {[taskId]: removed, ...rest} = prev.tasks`).
        let store = LOCAL_AGENT_TASK_STORES.lock().unwrap().remove(task_id);
        if let Some(store) = store {
            crate::utils::task::framework::remove_task(task_id, &store);
        }
    }
    removed
}

/// Maps to CC `LocalAgentTask.tsx#updateAgentProgress`.
fn abort_speculation_for_task_notification(task_id: &str) -> bool {
    let store = LOCAL_AGENT_TASK_STORES
        .lock()
        .unwrap()
        .get(task_id)
        .cloned();
    store.is_some_and(|store| {
        crate::services::prompt_suggestion::speculation::abort_speculation_for_store(&store)
    })
}

pub fn update_agent_progress(task_id: &str, progress: AgentProgress) {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return;
    };
    {
        let mut task = task.lock().unwrap();
        // CC `LocalAgentTask.tsx:436-438` — non-running updater returns the
        // same reference (no notify), so the mirror below is skipped too.
        if task.status != "running" {
            return;
        }
        let summary = task
            .progress
            .as_ref()
            .and_then(|progress| progress.summary.clone());
        task.progress = Some(AgentProgress {
            summary,
            ..progress
        });
    }
    // CC `LocalAgentTask.tsx:430-448` — updateAgentProgress IS a setAppState
    // (updateTaskState); mirror after releasing the task guard (lock
    // ordering, see `mirror_task_to_app_state`).
    mirror_task_to_app_state(task_id);
}

/// Maps to CC `LocalAgentTask.tsx:509-534` `completeAgentTask`.
///
/// Status transition ONLY. CC's closing note (`:533`) — "Notification is sent
/// by AgentTool via enqueueAgentNotification" — is the seam: the caller
/// enqueues separately so the classifier and worktree work the notification
/// depends on cannot gate the transition
/// (`AgentTool/agentToolUtils.ts:599-603`, gh-20236).
pub fn complete_agent_task(result: &CompletedAgentRun) {
    // CC `:513` `const taskId = result.agentId`.
    let task_id = result.agent_id.as_str();
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return;
    };
    // The output path is a symlink to the sidechain JSONL when persistence is
    // enabled. Never append clean answer text here: doing so would corrupt the
    // transcript. TaskOutput reads the clean in-memory result, matching CC.
    {
        let mut task = task.lock().unwrap();
        if task.status != "running" {
            return;
        }
        task.status = "completed".to_string();
        task.end_time_ms = Some(now_ms());
        // CC `:526` — `evictAfter: task.retain ? undefined : Date.now() +
        // PANEL_GRACE_MS`.
        task.evict_after =
            (!task.retain).then(|| now_ms() + crate::utils::task::framework::PANEL_GRACE_MS);
        task.result = Some(result.clone());
        task.selected_agent = None;
    }
    mirror_task_to_app_state(task_id);
}

/// Maps to CC `LocalAgentTask.tsx:539-564` `failAgentTask`. Status transition
/// only, same seam as [`complete_agent_task`].
pub fn fail_agent_task(task_id: &str, error: impl Into<String>) {
    let error = error.into();
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return;
    };
    // Do not append ad-hoc error text to the sidechain-transcript symlink.
    {
        let mut task = task.lock().unwrap();
        if task.status != "running" {
            return;
        }
        task.status = "failed".to_string();
        task.error = Some(error);
        task.end_time_ms = Some(now_ms());
        // CC `:556` — `evictAfter: task.retain ? undefined : Date.now() +
        // PANEL_GRACE_MS`.
        task.evict_after =
            (!task.retain).then(|| now_ms() + crate::utils::task::framework::PANEL_GRACE_MS);
        task.selected_agent = None;
    }
    mirror_task_to_app_state(task_id);
}

/// Maps to CC `LocalAgentTask.tsx:268-294` `enqueueAgentNotification` parameter
/// object.
///
/// CC's `description`/`toolUseId` parameters are read back off the registered
/// task here instead of being threaded: `registerAsyncAgent` was handed the
/// same two values its notification callers pass (`resumeAgent.ts:200`/`:204`
/// vs `agentToolUtils.ts:626`/`:635`).
pub struct EnqueueAgentNotificationParams<'a> {
    pub task_id: &'a str,
    /// CC `:282` `'completed' | 'failed' | 'killed'`.
    pub status: &'a str,
    pub error: Option<&'a str>,
    pub final_message: Option<&'a str>,
    /// CC `:286-290` `{ totalTokens, toolUses, durationMs }`.
    pub usage: Option<(u64, usize, u64)>,
    /// CC `:292-293` `worktreePath` + `worktreeBranch`; the path gates the
    /// whole section (`:334`).
    pub worktree: Option<(String, Option<String>)>,
}

/// Maps to CC `LocalAgentTask.tsx:268-346` `enqueueAgentNotification`.
///
/// The `notified` CAS (`:295-312`) belongs HERE, not with the status
/// transition: a `TaskOutput` retrieval that unblocks on the freshly terminal
/// status marks the task notified first (`TaskOutputTool.tsx:319-323`) and so
/// suppresses the now-redundant `<task-notification>` — CC's stated intent at
/// `:296-298`.
pub fn enqueue_agent_notification(params: EnqueueAgentNotificationParams<'_>) {
    let Some(task) = LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .get(params.task_id)
        .cloned()
    else {
        return;
    };
    let notification = {
        let mut task = task.lock().unwrap();
        if task.notified {
            return;
        }
        task.notified = true;
        agent_notification_xml(
            params.task_id,
            &task.description,
            params.status,
            params.error,
            params.final_message,
            params.usage,
            task.tool_use_id.as_deref(),
            params
                .worktree
                .as_ref()
                .map(|(path, branch)| (path.as_str(), branch.as_deref())),
        )
    };
    // CC `:299-308` — the notified flip goes through updateTaskState (an
    // AppState write) before abortSpeculation (`:317`).
    mirror_task_to_app_state(params.task_id);
    abort_speculation_for_task_notification(params.task_id);
    enqueue_agent_notification_xml(notification);
}

/// Maps to: CC `LocalAgentTask.tsx#getAllRunningAgentTasks`.
pub fn running_local_agent_tasks() -> Vec<LocalAgentTaskState> {
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .values()
        .filter_map(|task| {
            let task = task.lock().unwrap();
            (task.status == "running").then(|| task.clone())
        })
        .collect()
}

/// Snapshot all local-agent tasks for compact attachment regeneration.
/// Maps to CC `compact.ts:1571-1575` reading `AppState.tasks` and filtering
/// `type === 'local_agent'`.
pub fn local_agent_tasks_snapshot() -> Vec<LocalAgentTaskState> {
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .values()
        .map(|task| task.lock().unwrap().clone())
        .collect()
}

/// Maps to: CC `LocalAgentTask.tsx#killAllRunningAgentTasks` plus
/// `markAgentsNotified`. Returns the pre-kill snapshots used for the aggregate
/// model-facing notification.
pub fn kill_all_running_agent_tasks() -> Vec<LocalAgentTaskState> {
    let running = running_local_agent_tasks();
    let mut notification_store = None;
    for snapshot in &running {
        // Aggregate kill notification ordering matches CC task consumers:
        // terminal transition -> notified CAS -> speculation abort -> enqueue.
        if !kill_async_agent(&snapshot.task_id) {
            continue;
        }
        // Standalone binding: an `if let` scrutinee would extend the registry
        // MutexGuard across the whole block, and the mirror below re-locks the
        // same registry (std Mutex is non-reentrant → deadlock).
        let task = LOCAL_AGENT_TASKS
            .lock()
            .unwrap()
            .get(&snapshot.task_id)
            .cloned();
        if let Some(task) = task {
            let flipped = {
                let mut task = task.lock().unwrap();
                if task.notified {
                    false
                } else {
                    task.notified = true;
                    true
                }
            };
            if flipped {
                // Mirror the notified flip (kill_async_agent already mirrored
                // the terminal transition itself).
                mirror_task_to_app_state(&snapshot.task_id);
                if notification_store.is_none() {
                    notification_store = LOCAL_AGENT_TASK_STORES
                        .lock()
                        .unwrap()
                        .get(&snapshot.task_id)
                        .cloned();
                }
            }
        }
    }
    if let Some(store) = notification_store {
        crate::services::prompt_suggestion::speculation::abort_speculation_for_store(&store);
    }
    running
}

/// Maps to CC `LocalAgentTask.tsx#killAsyncAgent`.
pub fn kill_async_agent(task_id: &str) -> bool {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return false;
    };
    {
        let mut task = task.lock().unwrap();
        if task.status != "running" {
            return false;
        }
        task.abort_controller.abort();
        task.status = "killed".to_string();
        task.end_time_ms = Some(now_ms());
        // CC `:379` — `evictAfter: task.retain ? undefined : Date.now() +
        // PANEL_GRACE_MS`.
        task.evict_after =
            (!task.retain).then(|| now_ms() + crate::utils::task::framework::PANEL_GRACE_MS);
        task.selected_agent = None;
    }
    BACKGROUND_SIGNAL_RESOLVERS.lock().unwrap().remove(task_id);
    mirror_task_to_app_state(task_id);
    true
}

/// Remove a local_agent registry entry outright, returning the live state
/// handle so the caller can abort its controller. Rust-only carrier for the
/// `/clear` foreground-kill partition: CC deletes the AppState entry inside
/// the updater (commands/clear/conversation.ts:139-166) and every later
/// `updateTaskState` on the deleted id — the abort catch's killAsyncAgent,
/// the notification CAS (`LocalAgentTask.tsx:299-312`) — is then a no-op
/// (framework.ts:55-56). Without this removal the Rust registry keeps
/// answering for the id: the abort catch could CAS a ghost
/// `<task-notification>` into the post-clear session, and any later mirror
/// would resurrect the AppState entry the partition just dropped.
pub(crate) fn remove_local_agent_task_entry(
    task_id: &str,
) -> Option<Arc<Mutex<LocalAgentTaskState>>> {
    let task = LOCAL_AGENT_TASKS.lock().unwrap().remove(task_id);
    LOCAL_AGENT_TASK_STORES.lock().unwrap().remove(task_id);
    task
}

/// Atomically mark a local-agent task notified after TaskOutput retrieves a
/// terminal result. The Agent abort catch can still win this CAS and enqueue
/// its partial-result notification.
/// Maps to CC `TaskOutputTool.tsx:259-263,314-318` over TaskStateBase.notified.
pub fn mark_agent_task_notified(task_id: &str) -> bool {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return false;
    };
    {
        let mut task = task.lock().unwrap();
        if task.notified {
            return false;
        }
        task.notified = true;
    }
    mirror_task_to_app_state(task_id);
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalAgentTaskIdentitySnapshot {
    pub status: String,
    pub task_type: String,
    pub description: String,
}

/// Lightweight TaskOutput/TaskStop polling projection. This deliberately omits
/// message history and result payloads so a 100ms status poll remains O(1).
pub fn task_identity_snapshot(task_id: &str) -> Option<LocalAgentTaskIdentitySnapshot> {
    let task = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned()?;
    let task = task.lock().unwrap();
    Some(LocalAgentTaskIdentitySnapshot {
        status: task.status.clone(),
        task_type: task.task_type.clone(),
        description: task.description.clone(),
    })
}

pub fn get_local_agent_task(task_id: &str) -> Option<LocalAgentTaskState> {
    LOCAL_AGENT_TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
        .map(|task| task.lock().unwrap().clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueuePendingMessageResult {
    Queued,
    NotFound,
    NotRunning { status: String },
}

/// Maps to CC `LocalAgentTask.tsx#queuePendingMessage`.
pub fn queue_pending_message(
    task_id: &str,
    message: impl Into<String>,
) -> QueuePendingMessageResult {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return QueuePendingMessageResult::NotFound;
    };
    let mut task = task.lock().unwrap();
    if task.status != "running" {
        return QueuePendingMessageResult::NotRunning {
            status: task.status.clone(),
        };
    }
    task.pending_messages.push(message.into());
    QueuePendingMessageResult::Queued
}

/// Maps to CC `LocalAgentTask.tsx#appendMessageToLocalAgent`.
pub fn append_message_to_local_agent(task_id: &str, message: Message) -> bool {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return false;
    };
    let mut task = task.lock().unwrap();
    task.messages.push(message);
    true
}

/// Maps to CC `LocalAgentTask.tsx#drainPendingMessages`.
pub fn drain_pending_messages(task_id: &str) -> Vec<String> {
    let Some(task) = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned() else {
        return Vec::new();
    };
    let mut task = task.lock().unwrap();
    if task.pending_messages.is_empty() {
        return Vec::new();
    }
    std::mem::take(&mut task.pending_messages)
}

/// Maps to CC `TaskOutputTool.tsx#getTaskOutputData` local-agent branch.
pub fn task_output_snapshot(task_id: &str) -> Option<LocalAgentTaskOutputSnapshot> {
    let task = LOCAL_AGENT_TASKS.lock().unwrap().get(task_id).cloned()?;
    let (task_id, task_type, status, description, prompt, error, clean_result) = {
        let task = task.lock().unwrap();
        (
            task.task_id.clone(),
            task.task_type.clone(),
            task.status.clone(),
            task.description.clone(),
            task.prompt.clone(),
            task.error.clone(),
            task.result
                .as_ref()
                .map(|result| result.content.join("\n"))
                .filter(|result| !result.is_empty()),
        )
    };
    let disk_output = disk_output::get_task_output(&task_id, 8 * 1024 * 1024);
    // CC prefers the clean in-memory final assistant answer over the sidechain
    // JSONL symlink for both `output` and `result`.
    let output = clean_result.unwrap_or(disk_output);
    Some(LocalAgentTaskOutputSnapshot {
        task_id,
        task_type,
        status,
        description,
        result: Some(output.clone()),
        output,
        exit_code: None,
        error,
        prompt: Some(prompt),
    })
}

/// Maps to CC `LocalAgentTask.tsx#enqueueAgentNotification` message assembly.
pub fn agent_notification_xml(
    task_id: &str,
    description: &str,
    status: &str,
    error: Option<&str>,
    final_message: Option<&str>,
    usage: Option<(u64, usize, u64)>,
    tool_use_id: Option<&str>,
    worktree: Option<(&str, Option<&str>)>,
) -> String {
    let summary = match status {
        "completed" => format!("Agent \"{description}\" completed"),
        "failed" => format!(
            "Agent \"{description}\" failed: {}",
            error.unwrap_or("Unknown error")
        ),
        _ => format!("Agent \"{description}\" was stopped"),
    };
    let output_path = disk_output::get_task_output_path(task_id)
        .display()
        .to_string();
    let tool_use_id_line = tool_use_id
        .map(|id| format!("\n<{TOOL_USE_ID_TAG}>{id}</{TOOL_USE_ID_TAG}>"))
        .unwrap_or_default();
    let result_section = final_message
        .map(|message| format!("\n<result>{message}</result>"))
        .unwrap_or_default();
    let usage_section = usage
        .map(|(total_tokens, tool_uses, duration_ms)| {
            format!("\n<usage><total_tokens>{total_tokens}</total_tokens><tool_uses>{tool_uses}</tool_uses><duration_ms>{duration_ms}</duration_ms></usage>")
        })
        .unwrap_or_default();
    let worktree_section = worktree
        .map(|(path, branch)| {
            let branch = branch
                .map(|branch| format!("<{WORKTREE_BRANCH_TAG}>{branch}</{WORKTREE_BRANCH_TAG}>"))
                .unwrap_or_default();
            format!("\n<{WORKTREE_TAG}><{WORKTREE_PATH_TAG}>{path}</{WORKTREE_PATH_TAG}>{branch}</{WORKTREE_TAG}>")
        })
        .unwrap_or_default();

    format!(
        "<{TASK_NOTIFICATION_TAG}>\n<{TASK_ID_TAG}>{task_id}</{TASK_ID_TAG}>{tool_use_id_line}\n<{OUTPUT_FILE_TAG}>{output_path}</{OUTPUT_FILE_TAG}>\n<{STATUS_TAG}>{status}</{STATUS_TAG}>\n<{SUMMARY_TAG}>{summary}</{SUMMARY_TAG}>{result_section}{usage_section}{worktree_section}\n</{TASK_NOTIFICATION_TAG}>"
    )
}

/// Maps to CC `LocalAgentTask.tsx#enqueueAgentNotification` calling
/// `messageQueueManager.ts#enqueuePendingNotification`.
pub fn enqueue_agent_notification_xml(notification: String) {
    crate::utils::message_queue_manager::enqueue_pending_notification(
        crate::utils::message_queue_manager::QueuedCommand {
            value: notification,
            pre_expansion_value: None,
            pasted_contents: Default::default(),
            mode: "task-notification".to_string(),
            priority: crate::utils::message_queue_manager::QueuePriority::Later,
            agent_id: None,
            is_meta: true,
            uuid: None,
            skip_slash_commands: true,
        },
    );
}

#[cfg(test)]
pub fn clear_local_agent_tasks_for_test() {
    LOCAL_AGENT_TASKS.lock().unwrap().clear();
    LOCAL_AGENT_TASK_STORES.lock().unwrap().clear();
    BACKGROUND_SIGNAL_RESOLVERS.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
    use crate::types::message::{AssistantMessage, StopReason, TokenUsage, ToolUseBlock};
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn store_with_active_speculation()
    -> (crate::state::store::AppStore, crate::tool::AbortController) {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
        let cache = Arc::new(crate::utils::forked_agent::CacheSafeParams {
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context,
            fork_context_messages: Arc::new(Vec::new()),
        });
        let abort = crate::tool::AbortController::default();
        store.replace_with(|state| {
            state.prompt_suggestion.text = Some("run the tests".to_string());
            state.speculation = crate::state::app_state_store::SpeculationState::Active(
                crate::state::app_state_store::ActiveSpeculationState {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    abort_controller: abort.clone(),
                    start_time: 0,
                    messages: Arc::new(Mutex::new(Vec::new())),
                    written_paths: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
                    boundary: None,
                    suggestion_length: 13,
                    tool_use_count: 0,
                    is_pipelined: false,
                    cache_safe_params: cache,
                    pipelined_suggestion: None,
                },
            );
        });
        (store, abort)
    }

    #[test]
    fn progress_tracker_counts_tool_uses_and_latest_cumulative_tokens_like_official() {
        let mut tracker = create_progress_tracker();
        let message = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_read".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path":"Cargo.toml"}),
            })],
            model: Some("model".to_string()),
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 3,
                cache_creation_input_tokens: 2,
                cache_read_input_tokens: 1,
                cache_deleted_input_tokens: 0,
            }),
        });
        update_progress_from_message(&mut tracker, &message);
        let progress = get_progress_update(&tracker);
        assert_eq!(progress.tool_use_count, 1);
        assert_eq!(progress.token_count, 16);
        assert_eq!(progress.last_activity.unwrap().tool_name, "Read");
    }

    #[test]
    fn glob_progress_uses_tool_owned_activity_and_search_metadata() {
        let mut tracker = create_progress_tracker();
        let message = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_glob".to_string()),
                name: "Glob".to_string(),
                input: serde_json::json!({"pattern": "src/**/*.rs"}),
            })],
            model: Some("model".to_string()),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });
        update_progress_from_message(&mut tracker, &message);
        let activity = get_progress_update(&tracker)
            .last_activity
            .expect("Glob activity");
        assert_eq!(
            activity.activity_description.as_deref(),
            Some("Finding src/**/*.rs")
        );
        assert!(activity.is_search);
        assert!(!activity.is_read);
    }

    #[test]
    fn register_complete_and_task_output_snapshot_follow_local_agent_lifecycle() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let task = register_async_agent(RegisterAsyncAgentParams {
            agent_id: task_id.clone(),
            description: "inspect".to_string(),
            prompt: "read files".to_string(),
            selected_agent: agent,
            tool_use_id: Some("toolu_agent".to_string()),
        });
        assert_eq!(task.status, "running");
        assert!(std::path::Path::new(&task.output_file).exists());
        // Replace a possible transcript symlink with a deterministic fixture;
        // terminal transitions must not append raw answer text to this file.
        crate::utils::task::disk_output::cleanup_task_output(&task_id).unwrap();
        crate::utils::task::disk_output::init_task_output(&task_id).unwrap();
        std::fs::write(&task.output_file, "{\"type\":\"assistant\"}\n").unwrap();
        assert!(append_message_to_local_agent(
            &task_id,
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "queued".to_string()
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
            })
        ));
        assert_eq!(get_local_agent_task(&task_id).unwrap().messages.len(), 1);

        // CC `agentToolUtils.ts:603` then `:624-637`: the status transition and
        // the notification are two calls, in that order.
        complete_agent_task(&CompletedAgentRun {
            agent_id: task_id.clone(),
            agent_type: "general-purpose".to_string(),
            content: vec!["done".to_string()],
            messages: Vec::new(),
            total_tool_use_count: 0,
            total_duration_ms: 12,
            total_tokens: 34,
            usage: None,
            content_replacement_state: None,
        });
        enqueue_agent_notification(EnqueueAgentNotificationParams {
            task_id: &task_id,
            status: "completed",
            error: None,
            final_message: Some("done"),
            usage: Some((34, 0, 12)),
            worktree: None,
        });

        let snapshot = task_output_snapshot(&task_id).unwrap();
        assert_eq!(snapshot.task_type, "local_agent");
        assert_eq!(snapshot.status, "completed");
        assert_eq!(snapshot.result.as_deref(), Some("done"));
        assert_eq!(snapshot.output, "done");
        assert_eq!(
            std::fs::read_to_string(&task.output_file).unwrap(),
            "{\"type\":\"assistant\"}\n",
            "clean result text must not corrupt the sidechain transcript/output file"
        );
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1
        );
        let queued = crate::utils::message_queue_manager::dequeue(|_| true).unwrap();
        assert!(queued.value.contains("<task-notification>"));
        assert!(queued.value.contains("Agent \"inspect\" completed"));
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::utils::message_queue_manager::clear_command_queue();
    }

    #[test]
    fn first_agent_notification_aborts_speculation_but_preserves_suggestion() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let (store, speculation_abort) = store_with_active_speculation();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_async_agent_with_store(
            RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: AgentDefinition::new(
                    "general-purpose",
                    "Use for general tasks",
                    AgentDefinitionSource::BuiltIn,
                ),
                tool_use_id: Some("toolu_agent".to_string()),
            },
            Some(store.clone()),
        );

        complete_agent_task(&CompletedAgentRun {
            agent_id: task_id.clone(),
            agent_type: "general-purpose".to_string(),
            content: vec!["done".to_string()],
            messages: Vec::new(),
            total_tool_use_count: 0,
            total_duration_ms: 1,
            total_tokens: 2,
            usage: None,
            content_replacement_state: None,
        });
        assert!(
            !speculation_abort.is_aborted(),
            "CC aborts speculation inside enqueueAgentNotification (`:317`), not in completeAgentTask"
        );
        enqueue_agent_notification(EnqueueAgentNotificationParams {
            task_id: &task_id,
            status: "completed",
            error: None,
            final_message: Some("done"),
            usage: Some((2, 0, 1)),
            worktree: None,
        });

        assert!(speculation_abort.is_aborted());
        let state = store.get();
        assert!(matches!(
            state.speculation,
            crate::state::app_state_store::SpeculationState::Idle
        ));
        assert_eq!(
            state.prompt_suggestion.text.as_deref(),
            Some("run the tests")
        );
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1
        );
        complete_agent_task(&CompletedAgentRun {
            agent_id: task_id.clone(),
            agent_type: "general-purpose".to_string(),
            content: Vec::new(),
            messages: Vec::new(),
            total_tool_use_count: 0,
            total_duration_ms: 1,
            total_tokens: 0,
            usage: None,
            content_replacement_state: None,
        });
        enqueue_agent_notification(EnqueueAgentNotificationParams {
            task_id: &task_id,
            status: "completed",
            error: None,
            final_message: None,
            usage: None,
            worktree: None,
        });
        assert_eq!(
            crate::utils::message_queue_manager::get_command_queue_length(),
            1,
            "duplicate terminal transition must not enqueue or abort again"
        );
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::utils::message_queue_manager::clear_command_queue();
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn reregister_merges_ui_held_fields_like_cc_framework_87_to_97() {
        // CC utils/task/framework.ts:87-97 — re-registering an existing id
        // merges: retain/startTime/messages/diskLoaded/pendingMessages are
        // carried forward; everything else (status, description) refreshes.
        // The resume path (resume_agent.rs re-registering a live agent id)
        // depends on this for elapsed continuity and for queued pending
        // messages surviving the replace.
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_async_agent(RegisterAsyncAgentParams {
            agent_id: task_id.clone(),
            description: "first run".to_string(),
            prompt: "start".to_string(),
            selected_agent: agent.clone(),
            tool_use_id: None,
        });
        // Seed the merge-listed fields: a viewed-transcript message, a queued
        // pending message, and known startTime/retain/diskLoaded values.
        assert!(append_message_to_local_agent(
            &task_id,
            Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "appended prompt".to_string()
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
            })
        ));
        assert_eq!(
            queue_pending_message(&task_id, "queued prompt"),
            QueuePendingMessageResult::Queued
        );
        update_task_state_and_mirror(&task_id, |task| {
            task.start_time_ms = 12_345;
            task.retain = true;
            task.disk_loaded = true;
        });
        // Kill first so the status refresh on re-register is observable.
        assert!(kill_async_agent(&task_id));

        let resumed = register_async_agent(RegisterAsyncAgentParams {
            agent_id: task_id.clone(),
            description: "resumed run".to_string(),
            prompt: "continue".to_string(),
            selected_agent: agent,
            tool_use_id: None,
        });

        // CC :91-95 — the five carried-forward fields…
        assert_eq!(
            resumed.start_time_ms, 12_345,
            "startTime survives (elapsed continuity, stable panel sort)"
        );
        assert_eq!(resumed.messages.len(), 1, "viewed transcript survives");
        assert_eq!(
            resumed.pending_messages,
            vec!["queued prompt".to_string()],
            "queued pending messages survive the replace"
        );
        assert!(resumed.retain, "UI-held retain survives");
        assert!(resumed.disk_loaded);
        // …and the new state wins elsewhere (CC :87 `{...task, …}`).
        assert_eq!(resumed.status, "running");
        assert_eq!(resumed.description, "resumed run");
        assert_eq!(resumed.prompt, "continue");
        let stored = get_local_agent_task(&task_id).unwrap();
        assert_eq!(stored.start_time_ms, 12_345);
        assert_eq!(stored.description, "resumed run");
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn foreground_registration_background_and_unregister_follow_official_lifecycle() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let registration = register_agent_foreground(RegisterAgentForegroundParams {
            agent_id: task_id.clone(),
            description: "inspect".to_string(),
            prompt: "read files".to_string(),
            selected_agent: agent.clone(),
            auto_background_ms: None,
            tool_use_id: Some("toolu_agent".to_string()),
        });
        assert_eq!(registration.task_id, task_id);
        assert!(!*registration.background_signal.borrow());
        let task = get_local_agent_task(&task_id).unwrap();
        assert_eq!(task.status, "running");
        assert!(!task.is_backgrounded);
        assert!(background_agent_task(&task_id));
        assert!(*registration.background_signal.borrow());
        assert!(get_local_agent_task(&task_id).unwrap().is_backgrounded);
        assert!(!unregister_agent_foreground(&task_id));
        assert!(get_local_agent_task(&task_id).is_some());
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();

        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let registration = register_agent_foreground(RegisterAgentForegroundParams {
            agent_id: task_id.clone(),
            description: "inspect".to_string(),
            prompt: "read files".to_string(),
            selected_agent: agent.clone(),
            auto_background_ms: None,
            tool_use_id: None,
        });
        assert!(unregister_agent_foreground(&registration.task_id));
        assert!(get_local_agent_task(&task_id).is_none());
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);

        // CC LocalAgentTask.tsx:747 — `!isLocalAgentTask(task) ||
        // task.isBackgrounded` is the WHOLE gate: no status condition, so a
        // terminal (killed) foreground task can still be flipped.
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_agent_foreground(RegisterAgentForegroundParams {
            agent_id: task_id.clone(),
            description: "inspect".to_string(),
            prompt: "read files".to_string(),
            selected_agent: agent,
            auto_background_ms: None,
            tool_use_id: None,
        });
        assert!(kill_async_agent(&task_id));
        assert!(
            background_agent_task(&task_id),
            "terminal + not backgrounded still flips (CC has no status gate)"
        );
        assert!(get_local_agent_task(&task_id).unwrap().is_backgrounded);
        assert!(!background_agent_task(&task_id), "already backgrounded");
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn bulk_kill_aborts_all_running_agents_and_marks_aggregate_notified() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );
        let (store, speculation_abort) = store_with_active_speculation();
        for (id, description) in [("agent-one", "inspect"), ("agent-two", "test")] {
            register_async_agent_with_store(
                RegisterAsyncAgentParams {
                    agent_id: id.to_string(),
                    description: description.to_string(),
                    prompt: description.to_string(),
                    selected_agent: agent.clone(),
                    tool_use_id: None,
                },
                Some(store.clone()),
            );
        }

        let killed = kill_all_running_agent_tasks();
        assert!(speculation_abort.is_aborted());
        let app_state = store.get();
        assert_eq!(
            app_state.prompt_suggestion.text.as_deref(),
            Some("run the tests")
        );
        assert!(matches!(
            app_state.speculation,
            crate::state::app_state_store::SpeculationState::Idle
        ));
        assert_eq!(killed.len(), 2);
        for id in ["agent-one", "agent-two"] {
            let task = get_local_agent_task(id).unwrap();
            assert_eq!(task.status, "killed");
            assert!(task.notified);
            assert!(task.abort_controller.is_aborted());
            let _ = crate::utils::task::disk_output::cleanup_task_output(id);
        }
        assert!(running_local_agent_tasks().is_empty());
        clear_local_agent_tasks_for_test();
    }

    fn app_state_other(
        store: &crate::state::store::AppStore,
        task_id: &str,
    ) -> crate::state::app_state_store::TaskStateOther {
        let state = store.get();
        match state.tasks.get(task_id).map(|task| task.as_ref().clone()) {
            Some(crate::state::app_state_store::TaskState::Other(other)) => other,
            other => panic!("expected an Other mirror entry for {task_id}, got {other:?}"),
        }
    }

    #[test]
    fn async_lifecycle_mirrors_into_app_state_tasks() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );

        // CC `:627` registerTask — mirrored immediately with the required
        // `retain` field present.
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_async_agent_with_store(
            RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        let entry = app_state_other(&store, &task_id);
        assert_eq!(entry.task_type, "local_agent");
        assert_eq!(entry.status, "running");
        assert_eq!(entry.description, "inspect");
        assert_eq!(entry.is_backgrounded, Some(true));
        assert_eq!(entry.retain, Some(false));
        assert!(!entry.notified);
        assert_eq!(entry.evict_after, None);

        // CC `:526` completeAgentTask — terminal status + grace deadline.
        complete_agent_task(&CompletedAgentRun {
            agent_id: task_id.clone(),
            agent_type: "general-purpose".to_string(),
            content: vec!["done".to_string()],
            messages: Vec::new(),
            total_tool_use_count: 0,
            total_duration_ms: 1,
            total_tokens: 2,
            usage: None,
            content_replacement_state: None,
        });
        let entry = app_state_other(&store, &task_id);
        assert_eq!(entry.status, "completed");
        assert!(
            entry.evict_after.is_some(),
            "non-retained terminal task gets the PANEL_GRACE_MS deadline (CC :526)"
        );

        // CC `:299-308` enqueueAgentNotification — the notified flip reaches
        // the mirror before the queue write.
        enqueue_agent_notification(EnqueueAgentNotificationParams {
            task_id: &task_id,
            status: "completed",
            error: None,
            final_message: None,
            usage: None,
            worktree: None,
        });
        assert!(app_state_other(&store, &task_id).notified);

        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        crate::utils::message_queue_manager::clear_command_queue();
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn update_and_mirror_skips_the_store_install_when_nothing_changed() {
        // CC framework.ts:59-63 — an updater that changes nothing (CC: same
        // reference) does not notify s.tasks subscribers; the Rust carrier is
        // a value comparison that skips the mirror.
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_async_agent_with_store(
            RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: AgentDefinition::new(
                    "general-purpose",
                    "Use for general tasks",
                    AgentDefinitionSource::BuiltIn,
                ),
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        let revision = store.revision();
        update_task_state_and_mirror(&task_id, |_task| {});
        assert_eq!(
            store.revision(),
            revision,
            "no-change updater → no mirror, no install/notify"
        );
        update_task_state_and_mirror(&task_id, |task| {
            task.description = "renamed".to_string();
        });
        assert!(store.revision() > revision, "real change still mirrors");
        assert_eq!(app_state_other(&store, &task_id).description, "renamed");
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn foreground_mirror_flips_backgrounded_kills_and_unregisters() {
        let _task_lock = TEST_LOCAL_AGENT_TASK_LOCK.lock().unwrap();
        clear_local_agent_tasks_for_test();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let agent = AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            AgentDefinitionSource::BuiltIn,
        );

        // CC `:699` registerTask — foreground registration mirrors
        // `isBackgrounded: false`.
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_agent_foreground_with_store(
            RegisterAgentForegroundParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent.clone(),
                auto_background_ms: None,
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        assert_eq!(
            app_state_other(&store, &task_id).is_backgrounded,
            Some(false)
        );

        // CC `:752-764` backgroundAgentTask — the flip reaches the mirror.
        assert!(background_agent_task(&task_id));
        assert_eq!(
            app_state_other(&store, &task_id).is_backgrounded,
            Some(true)
        );

        // CC `:379` killAsyncAgent — killed + grace deadline in the mirror.
        assert!(kill_async_agent(&task_id));
        let entry = app_state_other(&store, &task_id);
        assert_eq!(entry.status, "killed");
        assert!(entry.evict_after.is_some());
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();

        // CC `:788-799` unregisterAgentForeground — the mirror entry is
        // removed with the registration.
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        register_agent_foreground_with_store(
            RegisterAgentForegroundParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                auto_background_ms: None,
                tool_use_id: None,
            },
            Some(store.clone()),
        );
        assert!(store.get().tasks.contains_key(&task_id));
        assert!(unregister_agent_foreground(&task_id));
        assert!(!store.get().tasks.contains_key(&task_id));
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
        clear_local_agent_tasks_for_test();
    }

    #[test]
    fn agent_notification_xml_matches_official_tag_shape() {
        let xml = agent_notification_xml(
            "agent-1",
            "inspect",
            "completed",
            None,
            Some("done"),
            Some((10, 2, 33)),
            Some("toolu_agent"),
            Some(("/tmp/worktree", Some("branch"))),
        );
        assert!(xml.starts_with("<task-notification>"));
        assert!(xml.contains("<task-id>agent-1</task-id>"));
        assert!(xml.contains("<tool-use-id>toolu_agent</tool-use-id>"));
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("<summary>Agent \"inspect\" completed</summary>"));
        assert!(xml.contains("<result>done</result>"));
        assert!(xml.contains("<total_tokens>10</total_tokens>"));
        assert!(xml.contains("<worktreePath>/tmp/worktree</worktreePath>"));
    }
}
