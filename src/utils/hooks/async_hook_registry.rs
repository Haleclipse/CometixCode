//! Pending async hook registry.
//! Maps to: CC `utils/hooks/AsyncHookRegistry.ts`.
//!
//! CC stores backgrounded `ShellCommand` objects and polls them for completed
//! sync JSON responses. Cometix has not ported the `ShellCommand` wrapper yet,
//! so this module stores an equivalent observable process handle (status,
//! stdout/stderr, exit code, kill/cleanup state). Hook orchestration can replace
//! that handle with a real process wrapper without changing the registry API.

use super::hook_events::{
    HookProgressInterval, HookProgressIntervalParams, HookProgressOutput, HookResponseEvent,
    HookResponseOutcome, emit_hook_response, start_hook_progress_interval,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maps to: CC async hook default timeout in `registerPendingAsyncHook`.
pub const DEFAULT_ASYNC_HOOK_TIMEOUT_MS: u64 = 15_000;

/// Maps to: CC `ShellCommand.status` values consumed by AsyncHookRegistry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncHookProcessStatus {
    Running,
    Completed,
    Killed,
}

#[derive(Clone, Debug)]
struct AsyncHookProcessState {
    status: AsyncHookProcessStatus,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    cleanup_called: bool,
}

impl Default for AsyncHookProcessState {
    fn default() -> Self {
        Self {
            status: AsyncHookProcessStatus::Running,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            cleanup_called: false,
        }
    }
}

/// Rust equivalent of the subset of CC `ShellCommand` consumed here.
#[derive(Clone, Debug, Default)]
pub struct AsyncHookProcessHandle {
    state: Arc<Mutex<AsyncHookProcessState>>,
}

impl AsyncHookProcessHandle {
    pub fn running() -> Self {
        Self::default()
    }

    pub fn completed(stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) -> Self {
        let handle = Self::default();
        handle.complete(stdout, stderr, exit_code);
        handle
    }

    pub fn complete(&self, stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) {
        let mut state = self.state.lock().expect("async hook process poisoned");
        state.status = AsyncHookProcessStatus::Completed;
        state.stdout = stdout.into();
        state.stderr = stderr.into();
        state.exit_code = Some(exit_code);
    }

    pub fn append_stdout(&self, value: &str) {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .stdout
            .push_str(value);
    }

    pub fn append_stderr(&self, value: &str) {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .stderr
            .push_str(value);
    }

    pub fn status(&self) -> AsyncHookProcessStatus {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .status
    }

    pub fn stdout(&self) -> String {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .stdout
            .clone()
    }

    pub fn stderr(&self) -> String {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .stderr
            .clone()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .exit_code
    }

    pub fn kill(&self) {
        let mut state = self.state.lock().expect("async hook process poisoned");
        if state.status != AsyncHookProcessStatus::Killed {
            state.status = AsyncHookProcessStatus::Killed;
            state.exit_code.get_or_insert(1);
        }
    }

    pub fn cleanup(&self) {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .cleanup_called = true;
    }

    #[cfg(test)]
    fn cleanup_called(&self) -> bool {
        self.state
            .lock()
            .expect("async hook process poisoned")
            .cleanup_called
    }
}

/// Maps to: CC `AsyncHookJSONOutput` fields used by the registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AsyncHookJsonOutput {
    pub async_timeout: Option<u64>,
}

/// Maps to: CC `PendingAsyncHook` public fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAsyncHook {
    pub process_id: String,
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub tool_name: Option<String>,
    pub plugin_id: Option<String>,
    pub start_time: u128,
    pub timeout: u64,
    pub command: String,
    pub response_attachment_sent: bool,
}

struct PendingAsyncHookEntry {
    snapshot: PendingAsyncHook,
    shell_command: Option<AsyncHookProcessHandle>,
    stop_progress_interval: Option<HookProgressInterval>,
}

/// Maps to: CC `registerPendingAsyncHook(...)` params.
pub struct RegisterPendingAsyncHookParams {
    pub process_id: String,
    pub hook_id: String,
    pub async_response: AsyncHookJsonOutput,
    pub hook_name: String,
    pub hook_event: String,
    pub command: String,
    pub shell_command: Option<AsyncHookProcessHandle>,
    pub tool_name: Option<String>,
    pub plugin_id: Option<String>,
}

/// Maps to: CC `checkForAsyncHookResponses()` response item.
#[derive(Clone, Debug, PartialEq)]
pub struct AsyncHookResponse {
    pub process_id: String,
    pub response: Value,
    pub hook_name: String,
    pub hook_event: String,
    pub tool_name: Option<String>,
    pub plugin_id: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

static PENDING_HOOKS: LazyLock<Mutex<HashMap<String, PendingAsyncHookEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn stop_interval(entry: &mut PendingAsyncHookEntry) {
    if let Some(interval) = entry.stop_progress_interval.take() {
        interval.stop();
    }
}

fn finalize_hook(
    entry: &mut PendingAsyncHookEntry,
    exit_code: i32,
    outcome: HookResponseOutcome,
) -> (String, String) {
    stop_interval(entry);
    let stdout = entry
        .shell_command
        .as_ref()
        .map(|process| process.stdout())
        .unwrap_or_default();
    let stderr = entry
        .shell_command
        .as_ref()
        .map(|process| process.stderr())
        .unwrap_or_default();
    if let Some(process) = &entry.shell_command {
        process.cleanup();
    }
    emit_hook_response(HookResponseEvent {
        hook_id: entry.snapshot.hook_id.clone(),
        hook_name: entry.snapshot.hook_name.clone(),
        hook_event: entry.snapshot.hook_event.clone(),
        output: format!("{stdout}{stderr}"),
        stdout: stdout.clone(),
        stderr: stderr.clone(),
        exit_code: Some(exit_code),
        outcome,
    });
    (stdout, stderr)
}

fn parse_sync_response_from_stdout(stdout: &str) -> Value {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            if parsed.get("async").is_none() {
                return parsed;
            }
        }
    }
    Value::Object(Default::default())
}

/// Maps to: CC `registerPendingAsyncHook(...)`.
pub fn register_pending_async_hook(params: RegisterPendingAsyncHookParams) {
    let timeout = params
        .async_response
        .async_timeout
        .unwrap_or(DEFAULT_ASYNC_HOOK_TIMEOUT_MS);

    let shell_for_progress = params.shell_command.clone();
    let stop_progress_interval = Some(start_hook_progress_interval(HookProgressIntervalParams {
        hook_id: params.hook_id.clone(),
        hook_name: params.hook_name.clone(),
        hook_event: params.hook_event.clone(),
        get_output: Arc::new(move || {
            let Some(process) = &shell_for_progress else {
                return HookProgressOutput::default();
            };
            let stdout = process.stdout();
            let stderr = process.stderr();
            HookProgressOutput {
                output: format!("{stdout}{stderr}"),
                stdout,
                stderr,
            }
        }),
        interval_ms: None,
    }));

    let process_id = params.process_id.clone();
    let entry = PendingAsyncHookEntry {
        snapshot: PendingAsyncHook {
            process_id: params.process_id,
            hook_id: params.hook_id,
            hook_name: params.hook_name,
            hook_event: params.hook_event,
            tool_name: params.tool_name,
            plugin_id: params.plugin_id,
            start_time: now_millis(),
            timeout,
            command: params.command,
            response_attachment_sent: false,
        },
        shell_command: params.shell_command,
        stop_progress_interval,
    };

    PENDING_HOOKS
        .lock()
        .expect("async hook registry poisoned")
        .insert(process_id, entry);
}

/// Maps to: CC `getPendingAsyncHooks()`.
pub fn get_pending_async_hooks() -> Vec<PendingAsyncHook> {
    PENDING_HOOKS
        .lock()
        .expect("async hook registry poisoned")
        .values()
        .filter(|entry| !entry.snapshot.response_attachment_sent)
        .map(|entry| entry.snapshot.clone())
        .collect()
}

/// Maps to: CC `checkForAsyncHookResponses()`.
pub async fn check_for_async_hook_responses() -> Vec<AsyncHookResponse> {
    let mut responses = Vec::new();
    let mut registry = PENDING_HOOKS.lock().expect("async hook registry poisoned");
    let process_ids = registry.keys().cloned().collect::<Vec<_>>();
    let mut remove_ids = Vec::new();

    for process_id in process_ids {
        let Some(entry) = registry.get_mut(&process_id) else {
            continue;
        };

        let Some(process) = entry.shell_command.clone() else {
            stop_interval(entry);
            remove_ids.push(process_id);
            continue;
        };

        match process.status() {
            AsyncHookProcessStatus::Killed => {
                stop_interval(entry);
                process.cleanup();
                remove_ids.push(process_id);
                continue;
            }
            AsyncHookProcessStatus::Running => continue,
            AsyncHookProcessStatus::Completed => {}
        }

        let stdout = process.stdout();
        let stderr = process.stderr();
        if entry.snapshot.response_attachment_sent || stdout.trim().is_empty() {
            stop_interval(entry);
            remove_ids.push(process_id);
            continue;
        }

        let exit_code = process.exit_code().unwrap_or(1);
        let response = parse_sync_response_from_stdout(&stdout);
        entry.snapshot.response_attachment_sent = true;
        let (stdout, stderr) = finalize_hook(
            entry,
            exit_code,
            if exit_code == 0 {
                HookResponseOutcome::Success
            } else {
                HookResponseOutcome::Error
            },
        );
        responses.push(AsyncHookResponse {
            process_id: process_id.clone(),
            response,
            hook_name: entry.snapshot.hook_name.clone(),
            hook_event: entry.snapshot.hook_event.clone(),
            tool_name: entry.snapshot.tool_name.clone(),
            plugin_id: entry.snapshot.plugin_id.clone(),
            stdout,
            stderr,
            exit_code: Some(exit_code),
        });
        remove_ids.push(process_id);
    }

    for process_id in remove_ids {
        registry.remove(&process_id);
    }

    // CC invalidates the session env cache when a SessionStart async hook
    // completes. Cometix has not ported sessionEnvironment yet; Cwd/File env
    // hook support remains tracked by the fileChangedWatcher/session-env slice.
    responses
}

/// Maps to: CC `removeDeliveredAsyncHooks(processIds)`.
pub fn remove_delivered_async_hooks(process_ids: &[String]) {
    let mut registry = PENDING_HOOKS.lock().expect("async hook registry poisoned");
    for process_id in process_ids {
        if registry
            .get(process_id)
            .is_some_and(|entry| entry.snapshot.response_attachment_sent)
        {
            if let Some(mut entry) = registry.remove(process_id) {
                stop_interval(&mut entry);
            }
        }
    }
}

/// Maps to: CC `finalizePendingAsyncHooks()`.
pub async fn finalize_pending_async_hooks() {
    let mut registry = PENDING_HOOKS.lock().expect("async hook registry poisoned");
    for entry in registry.values_mut() {
        let exit_code = match entry.shell_command.clone() {
            Some(process) if process.status() == AsyncHookProcessStatus::Completed => {
                process.exit_code().unwrap_or(1)
            }
            Some(process) => {
                if process.status() != AsyncHookProcessStatus::Killed {
                    process.kill();
                }
                1
            }
            None => 1,
        };
        let outcome = match entry.shell_command.as_ref().map(|process| process.status()) {
            Some(AsyncHookProcessStatus::Completed) if exit_code == 0 => {
                HookResponseOutcome::Success
            }
            Some(AsyncHookProcessStatus::Completed) => HookResponseOutcome::Error,
            _ => HookResponseOutcome::Cancelled,
        };
        finalize_hook(entry, exit_code, outcome);
    }
    registry.clear();
}

/// Maps to: CC `clearAllAsyncHooks()`.
pub fn clear_all_async_hooks() {
    let mut registry = PENDING_HOOKS.lock().expect("async hook registry poisoned");
    for entry in registry.values_mut() {
        stop_interval(entry);
    }
    registry.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hooks::hook_events::{
        HookEventHandler, HookExecutionEvent, clear_hook_event_state, register_hook_event_handler,
        set_all_hook_events_enabled,
    };
    use crate::utils::test_env::TestStateLock;

    static TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

    fn collect_handler(into: Arc<Mutex<Vec<HookExecutionEvent>>>) -> HookEventHandler {
        Arc::new(move |event| into.lock().expect("events mutex").push(event))
    }

    fn register(process_id: &str, process: Option<AsyncHookProcessHandle>, event: &str) {
        register_pending_async_hook(RegisterPendingAsyncHookParams {
            process_id: process_id.to_string(),
            hook_id: format!("hook-{process_id}"),
            async_response: AsyncHookJsonOutput {
                async_timeout: Some(42),
            },
            hook_name: "Async hook".to_string(),
            hook_event: event.to_string(),
            command: "echo async".to_string(),
            shell_command: process,
            tool_name: Some("Bash".to_string()),
            plugin_id: Some("plugin".to_string()),
        });
    }

    #[tokio::test]
    async fn completed_hook_returns_first_non_async_json_response_and_emits_event() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_all_async_hooks();
        clear_hook_event_state();
        set_all_hook_events_enabled(true);
        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));
        let process = AsyncHookProcessHandle::completed(
            "{\"async\":true}\n{\"systemMessage\":\"done\"}\nplain",
            "warn",
            0,
        );
        register("p1", Some(process.clone()), "PreToolUse");

        let pending = get_pending_async_hooks();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].timeout, 42);

        let responses = check_for_async_hook_responses().await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].process_id, "p1");
        assert_eq!(responses[0].response["systemMessage"], "done");
        assert_eq!(
            responses[0].stdout,
            "{\"async\":true}\n{\"systemMessage\":\"done\"}\nplain"
        );
        assert_eq!(responses[0].stderr, "warn");
        assert!(get_pending_async_hooks().is_empty());
        assert!(process.cleanup_called());

        let events = events.lock().expect("events mutex");
        assert!(events.iter().any(|event| matches!(
            event,
            HookExecutionEvent::Response(HookResponseEvent { outcome: HookResponseOutcome::Success, hook_event, .. }) if hook_event == "PreToolUse"
        )));
        clear_hook_event_state();
    }

    #[tokio::test]
    async fn running_no_process_and_killed_hooks_follow_official_registry_branches() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_all_async_hooks();
        clear_hook_event_state();
        let running = AsyncHookProcessHandle::running();
        let killed = AsyncHookProcessHandle::running();
        killed.kill();
        register("running", Some(running), "SessionStart");
        register("missing", None, "SessionStart");
        register("killed", Some(killed.clone()), "SessionStart");

        let responses = check_for_async_hook_responses().await;
        assert!(responses.is_empty());
        let pending = get_pending_async_hooks();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].process_id, "running");
        assert!(killed.cleanup_called());
        clear_all_async_hooks();
    }

    #[tokio::test]
    async fn completed_empty_stdout_is_removed_without_response() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_all_async_hooks();
        let process = AsyncHookProcessHandle::completed("   ", "", 0);
        register("empty", Some(process), "SessionStart");

        let responses = check_for_async_hook_responses().await;
        assert!(responses.is_empty());
        assert!(get_pending_async_hooks().is_empty());
    }

    #[tokio::test]
    async fn finalize_pending_hooks_cancels_running_and_clears_registry() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_all_async_hooks();
        clear_hook_event_state();
        set_all_hook_events_enabled(true);
        let events = Arc::new(Mutex::new(Vec::new()));
        register_hook_event_handler(Some(collect_handler(Arc::clone(&events))));
        let running = AsyncHookProcessHandle::running();
        running.append_stdout("partial");
        register("running", Some(running.clone()), "PreToolUse");

        finalize_pending_async_hooks().await;

        assert!(get_pending_async_hooks().is_empty());
        assert_eq!(running.status(), AsyncHookProcessStatus::Killed);
        assert!(running.cleanup_called());
        let events = events.lock().expect("events mutex");
        assert!(events.iter().any(|event| matches!(
            event,
            HookExecutionEvent::Response(HookResponseEvent { outcome: HookResponseOutcome::Cancelled, hook_event, .. }) if hook_event == "PreToolUse"
        )));
        clear_hook_event_state();
    }

    #[test]
    fn clear_all_async_hooks_stops_and_removes_pending_entries() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_all_async_hooks();
        register(
            "p1",
            Some(AsyncHookProcessHandle::running()),
            "SessionStart",
        );
        assert_eq!(get_pending_async_hooks().len(), 1);
        clear_all_async_hooks();
        assert!(get_pending_async_hooks().is_empty());
    }
}
