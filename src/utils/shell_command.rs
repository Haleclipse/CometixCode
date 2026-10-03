//! Process lifecycle for shell commands.
//!
//! Maps to: CC `utils/ShellCommand.ts:1-464`.

use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::utils::task::task_output::TaskOutput;

const SIGKILL_EXIT: i32 = 137;
const SIGTERM_EXIT: i32 = 143;
const SIZE_WATCHDOG_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(test)]
static TEST_SIZE_WATCHDOG_LIMIT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static TEST_SIZE_WATCHDOG_INTERVAL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn size_watchdog_limit() -> u64 {
    #[cfg(test)]
    {
        let limit = TEST_SIZE_WATCHDOG_LIMIT.load(Ordering::Acquire);
        if limit != 0 {
            return limit;
        }
    }
    crate::utils::task::disk_output::MAX_TASK_OUTPUT_BYTES
}

fn size_watchdog_interval() -> Duration {
    #[cfg(test)]
    {
        let interval_ms = TEST_SIZE_WATCHDOG_INTERVAL_MS.load(Ordering::Acquire);
        if interval_ms != 0 {
            return Duration::from_millis(interval_ms);
        }
    }
    SIZE_WATCHDOG_INTERVAL
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    /// Child stderr captured in the explicit no-write pipe fallback. Bash file
    /// mode has a single merged fd, so this is `None` there. Keeping the raw
    /// child bytes separate from synthetic timeout/watchdog diagnostics lets
    /// Bash reconstruct model-visible merged output without exposing lifecycle
    /// diagnostics that CC does not include in its merged command output.
    pub(crate) pipe_stderr: Option<String>,
    pub code: i32,
    pub interrupted: bool,
    pub background_task_id: Option<String>,
    pub backgrounded_by_user: bool,
    pub assistant_auto_backgrounded: bool,
    pub output_file_path: Option<String>,
    pub output_file_size: Option<u64>,
    pub output_task_id: Option<String>,
    /// Physical cwd captured by Shell.ts's `pwd -P` trailer.
    pub cwd_after: Option<std::path::PathBuf>,
    pub pre_spawn_error: Option<String>,
}

impl Default for ExecResult {
    fn default() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            pipe_stderr: None,
            code: 0,
            interrupted: false,
            background_task_id: None,
            backgrounded_by_user: false,
            assistant_auto_backgrounded: false,
            output_file_path: None,
            output_file_size: None,
            output_task_id: None,
            cwd_after: None,
            pre_spawn_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCommandStatus {
    Running,
    Backgrounded,
    Completed,
    Killed,
}

struct ResultState {
    value: Option<ExecResult>,
}

struct ShellCommandInner {
    status: Mutex<ShellCommandStatus>,
    background_task_id: Mutex<Option<String>>,
    backgrounded_by_user: AtomicBool,
    assistant_auto_backgrounded: AtomicBool,
    kill_requested: AtomicBool,
    forced_exit_code: AtomicI32,
    timeout_callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    timeout_callback_fired: AtomicBool,
    result: Mutex<ResultState>,
    result_ready: Condvar,
    task_output: TaskOutput,
}

/// Cloneable equivalent of CC's Promise-backed `ShellCommand` object.
#[derive(Clone)]
pub struct ShellCommand {
    inner: Arc<ShellCommandInner>,
}

impl std::fmt::Debug for ShellCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellCommand")
            .field("status", &self.status())
            .field("task_id", &self.task_output().task_id())
            .finish()
    }
}

impl PartialEq for ShellCommand {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for ShellCommand {}

impl ShellCommand {
    pub fn status(&self) -> ShellCommandStatus {
        self.inner
            .status
            .lock()
            .map(|status| *status)
            .unwrap_or(ShellCommandStatus::Killed)
    }

    pub fn task_output(&self) -> &TaskOutput {
        &self.inner.task_output
    }

    /// Maps to CC `onTimeout(callback)` registration.
    pub fn on_timeout(&self, callback: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut slot) = self.inner.timeout_callback.lock() {
            *slot = Some(Arc::new(callback));
        }
    }

    /// Maps to CC `background(backgroundTaskId)`.
    pub fn background(&self, task_id: &str) -> bool {
        let Ok(mut status) = self.inner.status.lock() else {
            return false;
        };
        if *status != ShellCommandStatus::Running {
            return false;
        }
        if let Ok(mut background_id) = self.inner.background_task_id.lock() {
            *background_id = Some(task_id.to_string());
        }
        *status = ShellCommandStatus::Backgrounded;
        drop(status);
        if !self.inner.task_output.stdout_to_file() {
            self.inner.task_output.spill_to_disk();
        }
        true
    }

    pub fn mark_backgrounded_by_user(&self) {
        self.inner
            .backgrounded_by_user
            .store(true, Ordering::Release);
    }

    pub fn mark_assistant_auto_backgrounded(&self) {
        self.inner
            .assistant_auto_backgrounded
            .store(true, Ordering::Release);
    }

    pub fn was_backgrounded_by_user(&self) -> bool {
        self.inner.backgrounded_by_user.load(Ordering::Acquire)
    }

    pub fn was_assistant_auto_backgrounded(&self) -> bool {
        self.inner
            .assistant_auto_backgrounded
            .load(Ordering::Acquire)
    }

    /// Maps to CC `kill()`.
    pub fn kill(&self) {
        if let Ok(mut status) = self.inner.status.lock() {
            *status = ShellCommandStatus::Killed;
        }
        self.inner
            .forced_exit_code
            .store(SIGKILL_EXIT, Ordering::Release);
        self.inner.kill_requested.store(true, Ordering::Release);
    }

    /// Wait for the official result promise without consuming it.
    pub fn wait_result(&self) -> ExecResult {
        let mut state = self
            .inner
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.value.is_none() {
            state = self
                .inner
                .result_ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.value.clone().unwrap_or_default()
    }

    pub fn wait_result_timeout(&self, timeout: Duration) -> Option<ExecResult> {
        let state = self
            .inner
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.value.is_some() {
            return state.value.clone();
        }
        let (state, _) = self
            .inner
            .result_ready
            .wait_timeout(state, timeout)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.value.clone()
    }

    /// Maps to CC `cleanup()`. Reader threads own no callback references after
    /// EOF; TaskOutput clears only in-memory pipe buffers here.
    pub fn cleanup(&self) {
        self.inner.task_output.clear();
        if let Ok(mut callback) = self.inner.timeout_callback.lock() {
            *callback = None;
        }
    }
}

/// Maps to CC `wrapSpawn(...)`.
pub type ResultFinalizer = Box<dyn FnOnce(&mut ExecResult) + Send + 'static>;

pub fn wrap_spawn(
    mut child: Child,
    abort_signal: crate::tool::AbortController,
    timeout: Duration,
    task_output: TaskOutput,
    should_auto_background: bool,
    result_finalizer: Option<ResultFinalizer>,
) -> ShellCommand {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let active_readers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    if let Some(stdout) = stdout {
        active_readers.fetch_add(1, Ordering::Relaxed);
        let output = task_output.clone();
        let readers = Arc::clone(&active_readers);
        std::thread::spawn(move || {
            drain_pipe(stdout, |chunk| output.write_stdout(chunk));
            readers.fetch_sub(1, Ordering::Release);
        });
    }
    if let Some(stderr) = stderr {
        active_readers.fetch_add(1, Ordering::Relaxed);
        let output = task_output.clone();
        let readers = Arc::clone(&active_readers);
        std::thread::spawn(move || {
            drain_pipe(stderr, |chunk| output.write_stderr(chunk));
            readers.fetch_sub(1, Ordering::Release);
        });
    }

    let inner = Arc::new(ShellCommandInner {
        status: Mutex::new(ShellCommandStatus::Running),
        background_task_id: Mutex::new(None),
        backgrounded_by_user: AtomicBool::new(false),
        assistant_auto_backgrounded: AtomicBool::new(false),
        kill_requested: AtomicBool::new(false),
        forced_exit_code: AtomicI32::new(0),
        timeout_callback: Mutex::new(None),
        timeout_callback_fired: AtomicBool::new(false),
        result: Mutex::new(ResultState { value: None }),
        result_ready: Condvar::new(),
        task_output,
    });
    let command = ShellCommand {
        inner: Arc::clone(&inner),
    };

    std::thread::spawn(move || {
        monitor_child(
            &mut child,
            &inner,
            &abort_signal,
            timeout,
            should_auto_background,
            &active_readers,
            result_finalizer,
        );
    });
    command
}

fn drain_pipe(mut pipe: impl std::io::Read, mut on_chunk: impl FnMut(&[u8])) {
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => on_chunk(&buffer[..read]),
        }
    }
}

fn monitor_child(
    child: &mut Child,
    inner: &Arc<ShellCommandInner>,
    abort_signal: &crate::tool::AbortController,
    timeout: Duration,
    should_auto_background: bool,
    active_readers: &std::sync::atomic::AtomicUsize,
    result_finalizer: Option<ResultFinalizer>,
) {
    let started = Instant::now();
    let mut last_size_watch = started;
    let mut killed_for_size = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => break synthetic_exit_status(1),
        }

        let state = inner
            .status
            .lock()
            .map(|status| *status)
            .unwrap_or(ShellCommandStatus::Killed);
        if inner.kill_requested.load(Ordering::Acquire) {
            mark_status_killed(inner);
            terminate_process_tree(child);
            break child
                .wait()
                .unwrap_or_else(|_| synthetic_exit_status(SIGKILL_EXIT));
        }

        if state == ShellCommandStatus::Backgrounded && inner.task_output.stdout_to_file() {
            if last_size_watch.elapsed() >= size_watchdog_interval() {
                last_size_watch = Instant::now();
                if std::fs::metadata(inner.task_output.path())
                    .is_ok_and(|metadata| metadata.len() > size_watchdog_limit())
                {
                    killed_for_size = true;
                    inner
                        .forced_exit_code
                        .store(SIGKILL_EXIT, Ordering::Release);
                    mark_status_killed(inner);
                    terminate_process_tree(child);
                    break child
                        .wait()
                        .unwrap_or_else(|_| synthetic_exit_status(SIGKILL_EXIT));
                }
            }
        } else if state == ShellCommandStatus::Running {
            if abort_signal.is_aborted() && abort_signal.reason().as_deref() != Some("interrupt") {
                inner
                    .forced_exit_code
                    .store(SIGKILL_EXIT, Ordering::Release);
                mark_status_killed(inner);
                terminate_process_tree(child);
                break child
                    .wait()
                    .unwrap_or_else(|_| synthetic_exit_status(SIGKILL_EXIT));
            }
            if started.elapsed() >= timeout {
                if should_auto_background {
                    let callback = inner
                        .timeout_callback
                        .lock()
                        .ok()
                        .and_then(|callback| callback.clone());
                    if let Some(callback) = callback {
                        if !inner.timeout_callback_fired.swap(true, Ordering::AcqRel) {
                            callback();
                        }
                    } else {
                        inner
                            .forced_exit_code
                            .store(SIGTERM_EXIT, Ordering::Release);
                        mark_status_killed(inner);
                        terminate_process_tree(child);
                        break child
                            .wait()
                            .unwrap_or_else(|_| synthetic_exit_status(SIGTERM_EXIT));
                    }
                } else {
                    inner
                        .forced_exit_code
                        .store(SIGTERM_EXIT, Ordering::Release);
                    mark_status_killed(inner);
                    terminate_process_tree(child);
                    break child
                        .wait()
                        .unwrap_or_else(|_| synthetic_exit_status(SIGTERM_EXIT));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    // Pipe mode waits briefly for final buffered chunks but never waits for a
    // grandchild that inherited the descriptors, matching CC's `exit` event.
    let drain_deadline = Instant::now() + Duration::from_millis(200);
    while active_readers.load(Ordering::Acquire) > 0 && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }

    finish_result(inner, status, killed_for_size, timeout, result_finalizer);
}

fn mark_status_killed(inner: &ShellCommandInner) {
    if let Ok(mut status) = inner.status.lock() {
        *status = ShellCommandStatus::Killed;
    }
}

fn finish_result(
    inner: &Arc<ShellCommandInner>,
    exit_status: ExitStatus,
    killed_for_size: bool,
    timeout: Duration,
    result_finalizer: Option<ResultFinalizer>,
) {
    let forced = inner.forced_exit_code.load(Ordering::Acquire);
    let code = if forced != 0 {
        forced
    } else {
        process_exit_code(exit_status)
    };
    let child_stderr = inner.task_output.get_stderr();
    let pipe_stderr = (!inner.task_output.stdout_to_file()).then(|| child_stderr.clone());
    let mut stderr = child_stderr;
    if killed_for_size {
        stderr = prepend_stderr(
            &format!(
                "Background command killed: output file exceeded {}",
                crate::utils::task::disk_output::MAX_TASK_OUTPUT_BYTES_DISPLAY
            ),
            &stderr,
        );
    } else if code == SIGTERM_EXIT {
        stderr = prepend_stderr(
            &format!("Command timed out after {}", format_duration(timeout)),
            &stderr,
        );
    }
    let background_task_id = inner
        .background_task_id
        .lock()
        .map(|id| id.clone())
        .unwrap_or_default();
    let stdout = inner.task_output.get_stdout();
    let mut result = ExecResult {
        stdout,
        stderr,
        pipe_stderr,
        code,
        interrupted: code == SIGKILL_EXIT,
        background_task_id: background_task_id.clone(),
        backgrounded_by_user: inner.backgrounded_by_user.load(Ordering::Acquire),
        assistant_auto_backgrounded: inner.assistant_auto_backgrounded.load(Ordering::Acquire),
        ..Default::default()
    };

    if inner.task_output.stdout_to_file() && background_task_id.is_none() {
        if inner.task_output.output_file_redundant() {
            inner.task_output.delete_output_file();
        } else {
            result.output_file_path = Some(inner.task_output.path().display().to_string());
            result.output_file_size = Some(inner.task_output.output_file_size());
            result.output_task_id = Some(inner.task_output.task_id().to_string());
        }
    }
    if let Some(finalize) = result_finalizer {
        finalize(&mut result);
    }

    if let Ok(mut status) = inner.status.lock() {
        if matches!(
            *status,
            ShellCommandStatus::Running | ShellCommandStatus::Backgrounded
        ) {
            *status = ShellCommandStatus::Completed;
        }
    }
    if let Ok(mut state) = inner.result.lock() {
        state.value = Some(result);
        inner.result_ready.notify_all();
    }
}

#[cfg(unix)]
fn process_exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().unwrap_or_else(|| {
        (status.signal() == Some(libc::SIGTERM))
            .then_some(144)
            .unwrap_or(1)
    })
}

#[cfg(windows)]
fn process_exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn prepend_stderr(prefix: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {stderr}")
    }
}

fn format_duration(duration: Duration) -> String {
    let milliseconds = duration.as_millis();
    if milliseconds >= 60_000 && milliseconds % 60_000 == 0 {
        format!("{}m", milliseconds / 60_000)
    } else if milliseconds >= 1_000 && milliseconds % 1_000 == 0 {
        format!("{}s", milliseconds / 1_000)
    } else {
        format!("{milliseconds}ms")
    }
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("taskkill");
        // tree-kill `exec('taskkill ...')` inherits process.env; the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        let _ = command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .status();
    }
    let _ = child.kill();
}

#[cfg(unix)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}

fn static_command(status: ShellCommandStatus, result: ExecResult) -> ShellCommand {
    let task_output = TaskOutput::new(
        crate::task::generate_task_id(crate::task::TaskType::LocalBash),
        false,
    );
    let inner = Arc::new(ShellCommandInner {
        status: Mutex::new(status),
        background_task_id: Mutex::new(result.background_task_id.clone()),
        backgrounded_by_user: AtomicBool::new(result.backgrounded_by_user),
        assistant_auto_backgrounded: AtomicBool::new(result.assistant_auto_backgrounded),
        kill_requested: AtomicBool::new(false),
        forced_exit_code: AtomicI32::new(result.code),
        timeout_callback: Mutex::new(None),
        timeout_callback_fired: AtomicBool::new(false),
        result: Mutex::new(ResultState {
            value: Some(result),
        }),
        result_ready: Condvar::new(),
        task_output,
    });
    ShellCommand { inner }
}

/// Maps to CC `createAbortedCommand(backgroundTaskId, opts)`.
pub fn create_aborted_command(
    background_task_id: Option<String>,
    stderr: Option<String>,
    code: Option<i32>,
) -> ShellCommand {
    static_command(
        ShellCommandStatus::Killed,
        ExecResult {
            code: code.unwrap_or(145),
            stderr: stderr.unwrap_or_else(|| "Command aborted before execution".to_string()),
            interrupted: true,
            background_task_id,
            ..Default::default()
        },
    )
}

/// Maps to CC `createFailedCommand(preSpawnError)`.
pub fn create_failed_command(error: impl Into<String>) -> ShellCommand {
    let error = error.into();
    static_command(
        ShellCommandStatus::Completed,
        ExecResult {
            code: 1,
            stderr: error.clone(),
            pre_spawn_error: Some(error),
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::process::{Command, Stdio};

    fn spawned(script: &str, timeout: Duration, auto_background: bool) -> ShellCommand {
        let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
        let output = TaskOutput::new(&task_id, false);
        let mut command = Command::new("bash");
        command
            .args(["-lc", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        wrap_spawn(
            command.spawn().unwrap(),
            crate::tool::AbortController::default(),
            timeout,
            output,
            auto_background,
            None,
        )
    }

    #[test]
    fn pipe_drain_preserves_partial_no_newline_output() {
        let command = spawned(
            "printf partial; printf err >&2",
            Duration::from_secs(2),
            false,
        );
        let result = command.wait_result();
        assert_eq!(result.stdout, "partial");
        assert_eq!(result.stderr, "err");
        assert_eq!(result.code, 0);
    }

    #[test]
    fn timeout_kills_process_tree_and_reports_official_code() {
        let command = spawned("sleep 30 & wait", Duration::from_millis(50), false);
        let result = command.wait_result();
        assert_eq!(result.code, SIGTERM_EXIT);
        assert_eq!(command.status(), ShellCommandStatus::Killed);
        assert!(result.stderr.contains("Command timed out after 50ms"));
    }

    #[test]
    fn background_disables_foreground_timeout() {
        let command = spawned("sleep .1; printf done", Duration::from_millis(20), true);
        let background = command.clone();
        command.on_timeout(move || {
            assert!(background.background("task-background"));
        });
        let result = command.wait_result();
        assert_eq!(result.code, 0);
        assert_eq!(
            result.background_task_id.as_deref(),
            Some("task-background")
        );
        assert_eq!(command.status(), ShellCommandStatus::Completed);
    }

    #[test]
    fn interrupt_abort_does_not_kill_foreground_shell() {
        let abort = crate::tool::AbortController::default();
        let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
        let output = TaskOutput::new(&task_id, false);
        let mut process = Command::new("bash");
        process
            .args(["-lc", "sleep .05; printf survived"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }
        let command = wrap_spawn(
            process.spawn().unwrap(),
            abort.clone(),
            Duration::from_secs(2),
            output,
            false,
            None,
        );
        abort.abort_with_reason("interrupt");
        let result = command.wait_result();
        assert_eq!(result.code, 0);
        assert_eq!(result.stdout, "survived");
    }

    #[test]
    fn low_threshold_background_watchdog_kills_and_retains_output_artifact() {
        struct OverrideGuard;
        impl Drop for OverrideGuard {
            fn drop(&mut self) {
                TEST_SIZE_WATCHDOG_LIMIT.store(0, Ordering::Release);
                TEST_SIZE_WATCHDOG_INTERVAL_MS.store(0, Ordering::Release);
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        TEST_SIZE_WATCHDOG_LIMIT.store(1_024, Ordering::Release);
        TEST_SIZE_WATCHDOG_INTERVAL_MS.store(10, Ordering::Release);
        let _override = OverrideGuard;

        let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
        let output = TaskOutput::new(&task_id, true);
        let output_path = output.path().to_path_buf();
        crate::utils::task::disk_output::init_task_output(&task_id).unwrap();
        let output_file = std::fs::OpenOptions::new()
            .append(true)
            .open(&output_path)
            .unwrap();
        let mut process = Command::new("bash");
        process
            .args(["-lc", "head -c 8192 /dev/zero; sleep 2"])
            .stdout(Stdio::from(output_file.try_clone().unwrap()))
            .stderr(Stdio::from(output_file));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }
        let command = wrap_spawn(
            process.spawn().unwrap(),
            crate::tool::AbortController::default(),
            Duration::from_secs(5),
            output,
            false,
            None,
        );
        assert!(command.background("watchdog-task"));
        let result = command.wait_result();
        assert_eq!(result.code, SIGKILL_EXIT);
        assert!(
            result
                .stderr
                .contains("Background command killed: output file exceeded 5GB")
        );
        assert!(std::fs::metadata(&output_path).is_ok_and(|metadata| metadata.len() >= 8_192));
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn pipe_mode_does_not_wait_for_grandchild_inherited_descriptors() {
        // Waiting on the grandchild would cost the full sleep (5s); not
        // waiting costs spawn+wait overhead. The 2s budget separates the two
        // by 3s — the old 1s-sleep/700ms-budget pair left only 300ms of
        // headroom and failed on spawn latency alone under a saturated
        // full-suite run.
        let started = Instant::now();
        let command = spawned(
            "(sleep 5; printf late) & printf early",
            Duration::from_secs(8),
            false,
        );
        let result = command.wait_result();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(result.stdout, "early");
    }
}
