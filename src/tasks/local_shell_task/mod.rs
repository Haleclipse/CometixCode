//! Local Bash task lifecycle.
//!
//! Maps to: CC `tasks/LocalShellTask/LocalShellTask.tsx:1-638`.

pub mod guards;
pub mod kill_shell_tasks;

use crate::state::app_state_store::{TaskState, TaskStateOther};
use crate::state::store::{AppStore, UpdateDecision};
use crate::utils::shell_command::{ExecResult, ShellCommand};
use guards::{LocalShellTaskResult, LocalShellTaskState};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub const BACKGROUND_BASH_SUMMARY_PREFIX: &str = "Background command ";
const STALL_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const STALL_THRESHOLD: Duration = Duration::from_secs(45);
const STALL_TAIL_BYTES: u64 = 1024;

/// Store locator only; canonical task state lives in `AppState.tasks`.
static TASK_STORES: LazyLock<Mutex<HashMap<String, AppStore>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static COMPLETION_HANDLERS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

#[derive(Clone, Debug)]
pub struct LocalShellSpawnInput {
    pub command: String,
    pub description: String,
    pub shell_command: ShellCommand,
    pub tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalShellTaskSnapshot {
    pub id: String,
    pub command: String,
    pub description: String,
    pub agent_id: Option<String>,
    pub status: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub notified: bool,
    pub is_backgrounded: bool,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn register_store(task_id: &str, store: &AppStore) {
    if let Ok(mut stores) = TASK_STORES.lock() {
        stores.insert(task_id.to_string(), store.clone());
    }
}

pub fn task_store(task_id: &str, preferred: Option<&AppStore>) -> Option<AppStore> {
    if let Some(store) = preferred {
        if store.get().tasks.contains_key(task_id) {
            return Some(store.clone());
        }
    }
    TASK_STORES
        .lock()
        .ok()
        .and_then(|stores| stores.get(task_id).cloned())
}

fn task_state(task_id: &str, preferred: Option<&AppStore>) -> Option<LocalShellTaskState> {
    let store = task_store(task_id, preferred)?;
    let state = store.get();
    match state.tasks.get(task_id)?.as_ref() {
        TaskState::LocalShell(task) => Some(task.clone()),
        _ => None,
    }
}

fn insert_task(store: &AppStore, task: LocalShellTaskState) {
    let id = task.id.clone();
    store.replace_with(|state| {
        // P4 identity: fresh map + fresh task Arc mirror CC's double spread.
        Arc::make_mut(&mut state.tasks).insert(id, Arc::new(TaskState::LocalShell(task)));
    });
}

fn update_task(
    task_id: &str,
    preferred: Option<&AppStore>,
    update: impl FnOnce(&mut LocalShellTaskState),
) -> bool {
    let Some(store) = task_store(task_id, preferred) else {
        return false;
    };
    // P3 §3a: CC `updateTask` returns `prev` untouched when the id is not a
    // LocalShell entry; Same carries the `false` result directly (the former
    // `changed` out-param).
    store.set_state(|prev| {
        if !matches!(
            prev.tasks.get(task_id).map(|task| task.as_ref()),
            Some(TaskState::LocalShell(_))
        ) {
            return UpdateDecision::Same(false);
        }
        let mut next = (**prev).clone();
        // P4 identity: make_mut(map) + make_mut(task Arc) yields fresh map
        // and per-task references — CC's `{...prev.tasks, [id]: {...task}}`.
        if let Some(entry) = Arc::make_mut(&mut next.tasks).get_mut(task_id) {
            if let TaskState::LocalShell(task) = Arc::make_mut(entry) {
                update(task);
            }
        }
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: true,
        }
    })
}

/// Maps to CC `looksLikePrompt(tail)`.
pub fn looks_like_prompt(tail: &str) -> bool {
    static PROMPT_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
        [
            r"(?i)\(y/n\)",
            r"(?i)\[y/n\]",
            r"(?i)\(yes/no\)",
            r"(?i)\b(?:Do you|Would you|Shall I|Are you sure|Ready to)\b.*\? *$",
            r"(?i)Press (?:any key|Enter)",
            r"(?i)Continue\?",
            r"(?i)Overwrite\?",
        ]
        .into_iter()
        .map(|pattern| regex::Regex::new(pattern).expect("valid prompt regex"))
        .collect()
    });
    let last = tail.trim_end().lines().last().unwrap_or_default();
    PROMPT_PATTERNS.iter().any(|pattern| pattern.is_match(last))
}

/// Maps to CC `startStallWatchdog(...)`.
fn start_stall_watchdog(task_id: String) -> Arc<std::sync::atomic::AtomicBool> {
    let skip =
        task_state(&task_id, None).is_some_and(|task| task.kind.as_deref() == Some("monitor"));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(skip));
    if skip {
        return cancelled;
    }
    let worker_cancelled = Arc::clone(&cancelled);
    std::thread::spawn(move || {
        let mut last_size = 0;
        let mut last_growth = std::time::Instant::now();
        while !worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(STALL_CHECK_INTERVAL);
            let Some(task) = task_state(&task_id, None) else {
                break;
            };
            if task.status != "running" || !task.is_backgrounded {
                break;
            }
            let size = crate::utils::task::disk_output::get_task_output_size(&task_id);
            if size > last_size {
                last_size = size;
                last_growth = std::time::Instant::now();
                continue;
            }
            if last_growth.elapsed() < STALL_THRESHOLD {
                continue;
            }
            let tail = crate::utils::task::disk_output::get_task_output(&task_id, STALL_TAIL_BYTES);
            if !looks_like_prompt(&tail) {
                last_growth = std::time::Instant::now();
                continue;
            }
            worker_cancelled.store(true, std::sync::atomic::Ordering::Release);
            let tool_use_id = task
                .tool_use_id
                .as_deref()
                .map(|id| format!("\n<tool-use-id>{id}</tool-use-id>"))
                .unwrap_or_default();
            let summary = format!(
                "{BACKGROUND_BASH_SUMMARY_PREFIX}\"{}\" appears to be waiting for interactive input",
                task.description
            );
            let output_path = crate::utils::task::disk_output::get_task_output_path(&task_id);
            let message = format!(
                "<task-notification>\n<task-id>{task_id}</task-id>{tool_use_id}\n<output-file>{}</output-file>\n<summary>{}</summary>\n</task-notification>\nLast output:\n{}\n\nThe command is likely blocked on an interactive prompt. Kill this task and re-run with piped input (e.g., `echo y | command`) or a non-interactive flag if one exists.",
                output_path.display(),
                crate::utils::xml::escape_xml(&summary),
                tail.trim_end(),
            );
            crate::utils::message_queue_manager::enqueue_pending_notification(
                crate::utils::message_queue_manager::QueuedCommand {
                    value: message,
                    pre_expansion_value: None,
                    pasted_contents: Default::default(),
                    mode: "task-notification".to_string(),
                    priority: crate::utils::message_queue_manager::QueuePriority::Next,
                    agent_id: task.agent_id,
                    is_meta: true,
                    uuid: None,
                    skip_slash_commands: true,
                },
            );
        }
    });
    cancelled
}

fn enqueue_shell_notification(task_id: &str, result: &ExecResult, store: &AppStore) {
    // Official ordering: terminal state → first notified CAS → speculation
    // abort → queue notification.
    // P3 §3a: a missing task or one already notified returns `prev`
    // untouched in CC; Same carries the None snapshot (former out-param).
    let snapshot = store.set_state(|prev| {
        match prev.tasks.get(task_id).map(|task| task.as_ref()) {
            Some(TaskState::LocalShell(task)) if !task.notified => {}
            _ => return UpdateDecision::Same(None),
        }
        let mut next = (**prev).clone();
        let snap = match Arc::make_mut(&mut next.tasks).get_mut(task_id) {
            Some(entry) => match Arc::make_mut(entry) {
                TaskState::LocalShell(task) => {
                    task.notified = true;
                    Some(task.clone())
                }
                _ => None,
            },
            None => None,
        };
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: snap,
        }
    });
    let Some(task) = snapshot else {
        return;
    };
    crate::services::prompt_suggestion::speculation::abort_speculation_for_store(store);

    let status = if task.status == "completed" {
        "completed"
    } else if task.status == "killed" {
        "killed"
    } else {
        "failed"
    };
    let summary = match status {
        "completed" => format!(
            "{BACKGROUND_BASH_SUMMARY_PREFIX}\"{}\" completed (exit code {})",
            task.description, result.code
        ),
        "failed" => format!(
            "{BACKGROUND_BASH_SUMMARY_PREFIX}\"{}\" failed with exit code {}",
            task.description, result.code
        ),
        _ => format!(
            "{BACKGROUND_BASH_SUMMARY_PREFIX}\"{}\" was stopped",
            task.description
        ),
    };
    let tool_use_id = task
        .tool_use_id
        .as_deref()
        .map(|id| format!("\n<tool-use-id>{id}</tool-use-id>"))
        .unwrap_or_default();
    let path = crate::utils::task::disk_output::get_task_output_path(task_id);
    let message = format!(
        "<task-notification>\n<task-id>{task_id}</task-id>{tool_use_id}\n<output-file>{}</output-file>\n<status>{status}</status>\n<summary>{}</summary>\n</task-notification>",
        path.display(),
        crate::utils::xml::escape_xml(&summary),
    );
    crate::utils::message_queue_manager::enqueue_pending_notification(
        crate::utils::message_queue_manager::QueuedCommand {
            value: message,
            pre_expansion_value: None,
            pasted_contents: Default::default(),
            mode: "task-notification".to_string(),
            // CC `feature('MONITOR_TOOL') ? 'next' : 'later'`
            // (LocalShellTask.tsx:222) — MONITOR_TOOL is off in the external
            // build, so 'later' is the reachable value.
            priority: crate::utils::message_queue_manager::QueuePriority::Later,
            agent_id: task.agent_id,
            is_meta: true,
            uuid: None,
            skip_slash_commands: true,
        },
    );
}

fn attach_completion_handler(task_id: &str, shell_command: ShellCommand, store: AppStore) {
    let should_attach = COMPLETION_HANDLERS
        .lock()
        .map(|mut handlers| handlers.insert(task_id.to_string()))
        .unwrap_or(false);
    if !should_attach {
        return;
    }
    let task_id = task_id.to_string();
    let stall_cancelled = start_stall_watchdog(task_id.clone());
    std::thread::spawn(move || {
        let result = shell_command.wait_result();
        stall_cancelled.store(true, std::sync::atomic::Ordering::Release);
        // Maps to CC `flushAndCleanup(shellCommand)` before the AppState
        // terminal transition and notification. This also gives the foreground
        // Bash result owner a chance to mark a raced background task notified.
        let _ = futures::executor::block_on(shell_command.task_output().flush());
        shell_command.cleanup();
        // P3 §3a: a task evicted before completion returns `prev` untouched
        // in CC; Same carries `false` (the former `was_killed` out-param).
        let was_killed = store.set_state(|prev| {
            if !matches!(
                prev.tasks.get(&task_id).map(|task| task.as_ref()),
                Some(TaskState::LocalShell(_))
            ) {
                return UpdateDecision::Same(false);
            }
            let mut next = (**prev).clone();
            let mut was_killed = false;
            if let Some(entry) = Arc::make_mut(&mut next.tasks).get_mut(&task_id) {
                if let TaskState::LocalShell(task) = Arc::make_mut(entry) {
                    if task.status == "killed" {
                        was_killed = true;
                    } else {
                        task.status = if result.code == 0 {
                            "completed".to_string()
                        } else {
                            "failed".to_string()
                        };
                    }
                    task.result = Some(LocalShellTaskResult {
                        code: result.code,
                        interrupted: result.interrupted,
                    });
                    task.shell_command = None;
                    task.end_time_ms = Some(now_ms());
                }
            }
            UpdateDecision::Replace {
                next: Arc::new(next),
                result: was_killed,
            }
        });
        if was_killed {
            update_task(&task_id, Some(&store), |task| {
                task.status = "killed".to_string();
            });
        }
        enqueue_shell_notification(&task_id, &result, &store);
        if let Ok(mut handlers) = COMPLETION_HANDLERS.lock() {
            handlers.remove(&task_id);
        }
    });
}

fn state_from_input(input: &LocalShellSpawnInput, backgrounded: bool) -> LocalShellTaskState {
    LocalShellTaskState {
        id: input.shell_command.task_output().task_id().to_string(),
        task_type: crate::task::TaskType::LocalBash.as_str().to_string(),
        status: "running".to_string(),
        description: input.description.clone(),
        command: input.command.clone(),
        result: None,
        notified: false,
        shell_command: Some(input.shell_command.clone()),
        last_reported_total_lines: 0,
        is_backgrounded: backgrounded,
        agent_id: input.agent_id.clone(),
        tool_use_id: input.tool_use_id.clone(),
        kind: input.kind.clone(),
        start_time_ms: now_ms(),
        end_time_ms: None,
    }
}

/// Maps to CC `spawnShellTask(input, context)`.
pub fn spawn_shell_task(input: LocalShellSpawnInput, store: &AppStore) -> Result<String, String> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(
            crate::tools::shared::write_gate::BACKGROUND_COMMAND_DISABLED_ERROR.to_string(),
        );
    }
    let task_id = input.shell_command.task_output().task_id().to_string();
    insert_task(store, state_from_input(&input, true));
    register_store(&task_id, store);
    // CC deliberately ignores the boolean here: a command may settle in the
    // narrow window after task registration. Its result handler still owns the
    // terminal transition and notification.
    let _ = input.shell_command.background(&task_id);
    attach_completion_handler(&task_id, input.shell_command, store.clone());
    Ok(task_id)
}

/// Maps to CC `registerForeground(input, setAppState, toolUseId)`.
pub fn register_foreground(input: LocalShellSpawnInput, store: &AppStore) -> String {
    let task_id = input.shell_command.task_output().task_id().to_string();
    insert_task(store, state_from_input(&input, false));
    register_store(&task_id, store);
    task_id
}

/// Maps to CC private `backgroundTask(...)`.
fn background_task(task_id: &str, by_user: bool) -> bool {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return false;
    }
    let Some(store) = task_store(task_id, None) else {
        return false;
    };
    let Some(task) = task_state(task_id, Some(&store)) else {
        return false;
    };
    if task.is_backgrounded || task.status != "running" {
        return false;
    }
    let Some(shell_command) = task.shell_command else {
        return false;
    };
    if !shell_command.background(task_id) {
        return false;
    }
    if by_user {
        shell_command.mark_backgrounded_by_user();
    }
    update_task(task_id, Some(&store), |task| task.is_backgrounded = true);
    attach_completion_handler(task_id, shell_command, store);
    true
}

/// Maps to CC `backgroundExistingForegroundTask(...)`. Rust retrieves the
/// already-registered ShellCommand from canonical AppState instead of receiving
/// the same handle again as an argument.
pub fn background_existing_foreground_task(task_id: &str, by_user: bool) -> bool {
    background_task(task_id, by_user)
}

/// Maps to CC `LocalShellTask.tsx:471-490#hasForegroundTasks`.
pub fn has_foreground_tasks(state: &crate::state::app_state_store::AppState) -> bool {
    state.tasks.values().any(|task| match task.as_ref() {
        TaskState::LocalShell(shell) => !shell.is_backgrounded && shell.shell_command.is_some(),
        TaskState::Other(agent)
            if crate::tasks::local_agent_task::is_local_agent_task(&agent.task_type) =>
        {
            // The frame owns membership/background state. Only agentType is
            // absent from the existing Other projection and needs its registry.
            agent.is_backgrounded != Some(true)
                && !crate::tasks::local_agent_task::get_local_agent_task(&agent.id).is_some_and(
                    |task| crate::tasks::local_main_session_task::is_main_session_task(&task),
                )
        }
        _ => false,
    })
}

/// Maps to CC `LocalShellTask.tsx:492-516#backgroundAll`.
/// Both task families are selected from the same AppState snapshot before
/// mutation, matching the source's getAppState followed by shell/agent loops.
pub fn background_all(store: &AppStore) -> bool {
    let state = store.get();
    let shell_ids = state
        .tasks
        .values()
        .filter_map(|task| match task.as_ref() {
            TaskState::LocalShell(shell)
                if !shell.is_backgrounded && shell.shell_command.is_some() =>
            {
                Some(shell.id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let agent_ids = state
        .tasks
        .values()
        .filter_map(|task| match task.as_ref() {
            TaskState::Other(agent)
                if crate::tasks::local_agent_task::is_local_agent_task(&agent.task_type)
                    && agent.is_backgrounded != Some(true) =>
            {
                Some(agent.id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut changed = shell_ids
        .into_iter()
        .fold(false, |changed, id| background_task(&id, true) || changed);
    for id in agent_ids {
        changed |= crate::tasks::local_agent_task::background_agent_task(&id);
    }
    changed
}

/// Maps to CC `markTaskNotified(taskId, setAppState)`.
pub fn mark_task_notified(task_id: &str) -> bool {
    let mut changed = false;
    update_task(task_id, None, |task| {
        if !task.notified {
            task.notified = true;
            changed = true;
        }
    });
    changed
}

/// Maps to CC `unregisterForeground(taskId, setAppState)`.
pub fn unregister_foreground(task_id: &str) {
    let Some(store) = task_store(task_id, None) else {
        return;
    };
    // P3 §3a: only a live foreground LocalShell entry is removed; anything
    // else returns `prev` untouched (CC returns state unchanged).
    store.set_state(|prev| {
        if !prev.tasks.get(task_id).is_some_and(
            |task| matches!(task.as_ref(), TaskState::LocalShell(shell) if !shell.is_backgrounded),
        ) {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        Arc::make_mut(&mut next.tasks).remove(task_id);
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });
    if let Ok(mut stores) = TASK_STORES.lock() {
        stores.remove(task_id);
    }
}

pub fn task_identity_snapshot(
    task_id: &str,
    preferred: Option<&AppStore>,
) -> Option<crate::tasks::stop_task::TaskLookupSnapshot> {
    let task = task_state(task_id, preferred)?;
    Some(crate::tasks::stop_task::TaskLookupSnapshot {
        status: task.status,
        task_type: task.task_type,
        description: task.description,
        command: Some(task.command),
    })
}

pub fn task_output_snapshot(
    task_id: &str,
    preferred: Option<&AppStore>,
) -> Option<LocalShellTaskSnapshot> {
    let task = task_state(task_id, preferred)?;
    let output = crate::utils::task::disk_output::get_task_output(task_id, 8 * 1024 * 1024);
    Some(LocalShellTaskSnapshot {
        id: task.id,
        command: task.command,
        description: task.description,
        agent_id: task.agent_id,
        status: task.status,
        output,
        exit_code: task.result.map(|result| result.code),
        notified: task.notified,
        is_backgrounded: task.is_backgrounded,
    })
}

pub fn all_task_snapshots() -> Vec<LocalShellTaskSnapshot> {
    let stores = TASK_STORES
        .lock()
        .map(|stores| stores.clone())
        .unwrap_or_default();
    let mut snapshots = stores
        .into_iter()
        .filter_map(|(id, store)| task_output_snapshot(&id, Some(&store)))
        .collect::<Vec<_>>();
    snapshots.sort_by(|a, b| a.id.cmp(&b.id));
    snapshots
}

/// Compatibility projection for AppState consumers that do not yet understand
/// the concrete variant.
pub fn as_other_state(task: &LocalShellTaskState) -> TaskStateOther {
    TaskStateOther {
        id: task.id.clone(),
        task_type: task.task_type.clone(),
        status: task.status.clone(),
        description: task.description.clone(),
        is_backgrounded: Some(task.is_backgrounded),
        notified: task.notified,
        // `retain`/`evictAfter`/`progress` are LocalAgentTaskState-only
        // fields (absent on shell tasks, CC framework.ts:138 narrowing).
        retain: None,
        evict_after: None,
        progress_tool_uses: None,
        progress_tokens: None,
    }
}

#[cfg(test)]
pub fn clear_for_test() {
    if let Ok(mut stores) = TASK_STORES.lock() {
        stores.clear();
    }
    if let Ok(mut handlers) = COMPLETION_HANDLERS.lock() {
        handlers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct EnvGuard {
        _env: EnvVarGuard,
    }

    impl EnvGuard {
        fn writes(value: &str) -> Self {
            Self {
                _env: EnvVarGuard::set("COMETIX_WRITE_ENABLED", value),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            kill_shell_tasks::kill_all_shell_tasks();
            clear_for_test();
        }
    }

    #[test]
    fn prompt_detection_matches_official_patterns() {
        assert!(looks_like_prompt("Install package? (Y/n)"));
        assert!(looks_like_prompt("Press Enter"));
        assert!(!looks_like_prompt("notdo you want this?"));
        assert!(!looks_like_prompt("building package 42%"));
    }

    #[test]
    fn ctrl_b_backgrounds_registered_foreground_task_in_place() {
        use std::process::{Command, Stdio};

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvGuard::writes("1");
        clear_for_test();
        let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
        let output = crate::utils::task::task_output::TaskOutput::new(&task_id, false);
        let mut process = Command::new("bash");
        process
            .args(["-lc", "sleep .2; printf ctrl-b-done"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            process.process_group(0);
        }
        let shell = crate::utils::shell_command::wrap_spawn(
            process.spawn().unwrap(),
            crate::tool::AbortController::default(),
            Duration::from_secs(30),
            output,
            true,
            None,
        );
        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        let id = register_foreground(
            LocalShellSpawnInput {
                command: "sleep .2; printf ctrl-b-done".to_string(),
                description: "Ctrl-B task".to_string(),
                shell_command: shell.clone(),
                tool_use_id: Some("toolu_ctrl_b".to_string()),
                agent_id: None,
                kind: None,
            },
            &store,
        );
        assert!(has_foreground_tasks(&store.get()));
        assert!(background_all(&store));
        assert!(!has_foreground_tasks(&store.get()));
        assert_eq!(
            shell.status(),
            crate::utils::shell_command::ShellCommandStatus::Backgrounded
        );
        assert!(shell.was_backgrounded_by_user());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline
            && task_state(&id, Some(&store))
                .is_some_and(|task| task.status == "running" || !task.notified)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let task = task_state(&id, Some(&store)).expect("task retained");
        assert_eq!(task.status, "completed");
        assert!(task.notified);
        assert!(
            crate::utils::task::disk_output::get_task_output(&id, 1024).contains("ctrl-b-done")
        );

        let _ = std::fs::remove_file(crate::utils::task::disk_output::get_task_output_path(&id));
        crate::utils::message_queue_manager::dequeue_all_matching(|queued| {
            queued.value.contains(&id)
        });
    }

    #[test]
    fn no_write_mode_refuses_explicit_and_in_place_backgrounding() {
        use std::process::{Command, Stdio};

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvGuard::writes("0");
        clear_for_test();
        let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
        let output = crate::utils::task::task_output::TaskOutput::new(&task_id, false);
        let mut process = Command::new("bash");
        process
            .args(["-lc", "sleep 5"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            process.process_group(0);
        }
        let shell = crate::utils::shell_command::wrap_spawn(
            process.spawn().unwrap(),
            crate::tool::AbortController::default(),
            Duration::from_secs(30),
            output,
            true,
            None,
        );
        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        let input = LocalShellSpawnInput {
            command: "sleep 5".to_string(),
            description: "sleep".to_string(),
            shell_command: shell.clone(),
            tool_use_id: None,
            agent_id: None,
            kind: None,
        };

        let error = spawn_shell_task(input.clone(), &store).unwrap_err();
        assert_eq!(
            error,
            crate::tools::shared::write_gate::BACKGROUND_COMMAND_DISABLED_ERROR
        );
        let id = register_foreground(input, &store);
        assert!(!background_existing_foreground_task(&id, true));
        assert_eq!(
            shell.status(),
            crate::utils::shell_command::ShellCommandStatus::Running
        );

        shell.kill();
        let _ = shell.wait_result();
        shell.cleanup();
        unregister_foreground(&id);
        assert!(!shell.task_output().path().exists());
    }
}
