//! No-throw subprocess wrapper.
//!
//! Maps to: CC `utils/execFileNoThrow.ts`.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// L1 injection of `osc.ts`'s imported `execFileNoThrow` and Node event loop
/// into the iocraft replacement. Clipboard policy and encoding stay in iocraft.
/// Maps to: CC `ink/termio/osc.ts:6,96-101,174-225`.
pub(crate) struct ExecFileClipboardBackend;

impl iocraft::ClipboardBackend for ExecFileClipboardBackend {
    fn execute(
        &self,
        program: &str,
        args: &[&str],
        input: &str,
        timeout: Duration,
    ) -> futures::future::BoxFuture<'static, i32> {
        // Launch before returning the future, as the original Promise executor
        // does. Dropping a native-copy observer does not stop its process.
        let process = exec_file_no_throw_with_input(program, args, timeout, input);
        Box::pin(async move { process.await.code })
    }

    fn spawn(&self, future: futures::future::BoxFuture<'static, ()>) {
        crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("clipboard requires the initialized process runtime")
            .spawn(future);
    }

    fn is_kitty(&self) -> bool {
        crate::utils::env::get().terminal.as_deref() == Some("kitty")
    }
}

pub(crate) struct ExecFileOutput {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) error: Option<String>,
}

impl ExecFileOutput {
    // Empty result carrier used for catch/worker failures and the legacy
    // synchronous spawn adapter; async spawn failures add their source error.
    fn failed() -> Self {
        Self {
            code: 1,
            stdout: String::new(),
            stderr: String::new(),
            error: None,
        }
    }
}

/// Maps to: CC `utils/execFileNoThrow.ts#getErrorMessage:73-85`.
fn get_error_message(short_message: Option<&str>, signal: Option<&str>, error_code: i32) -> String {
    if let Some(message) = short_message.filter(|message| !message.is_empty()) {
        return message.to_owned();
    }
    if let Some(signal) = signal {
        return signal.to_owned();
    }
    error_code.to_string()
}

// Native Execa escapedCommand boundary (arguments/escape.js), not a CC
// function. Diagnostics quote argv; actual Command receives the original argv.
fn native_execa_command(program: &str, args: &[&str]) -> String {
    static SPECIAL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[\p{Z}\p{C}]").unwrap());
    std::iter::once(program)
        .chain(args.iter().copied())
        .map(|argument| {
            let escaped = SPECIAL.replace_all(argument, |captures: &regex::Captures<'_>| {
                let ch = captures[0].chars().next().unwrap();
                match ch {
                    ' ' => " ".to_owned(),
                    '\u{8}' => r"\b".to_owned(),
                    '\u{c}' => r"\f".to_owned(),
                    '\n' => r"\n".to_owned(),
                    '\r' => r"\r".to_owned(),
                    '\t' => r"\t".to_owned(),
                    ch if ch as u32 <= 65535 => format!("\\u{:04x}", ch as u32),
                    ch => format!("\\U{:x}", ch as u32),
                }
            });
            if !escaped.is_empty()
                && escaped
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_./-".contains(&c))
            {
                escaped.into_owned()
            } else {
                format!("'{}'", escaped.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Maps to: CC `utils/execFileNoThrow.ts#execFileNoThrow`.
pub(crate) fn exec_file_no_throw(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> ExecFileOutput {
    exec_file_no_throw_with_cwd(program, args, timeout, None, true)
}

/// Maps to: CC `utils/execFileNoThrow.ts#execFileNoThrowWithCwd`.
pub(crate) fn exec_file_no_throw_with_cwd(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&std::path::Path>,
    preserve_output_on_error: bool,
) -> ExecFileOutput {
    match StartedProcess::spawn_options(
        program,
        args,
        &ExecFileWithCwdOptions {
            timeout,
            cwd,
            stdin: ExecFileStdin::Inherit,
            ..Default::default()
        },
        &crate::utils::process_env::snapshot(),
    ) {
        Ok(process) => process.finish(preserve_output_on_error),
        Err(_) => ExecFileOutput::failed(),
    }
}

/// Maps to: CC `utils/execFileNoThrow.ts#ExecFileWithCwdOptions:45-56`.
/// Borrowed native option carrier for the verified subset. Duration excludes
/// negative timeouts; stdin's tagged form excludes Execa-rejected input/ignore
/// combinations. Valid inherit+input is also outside this typed subset.
/// Abort, shell and maxBuffer remain unported options.
pub(crate) struct ExecFileWithCwdOptions<'a> {
    pub(crate) timeout: Duration,
    pub(crate) preserve_output_on_error: bool,
    pub(crate) cwd: Option<&'a std::path::Path>,
    /// None value represents a JS own property with value undefined (delete
    /// from the inherited process.env); an absent entry still inherits.
    pub(crate) env: &'a [(std::ffi::OsString, Option<std::ffi::OsString>)],
    pub(crate) stdin: ExecFileStdin<'a>,
}

/// Native representation of valid `stdin`/`input` combinations, owned by the
/// source options object rather than an independent input policy.
#[derive(Clone, Copy)]
pub(crate) enum ExecFileStdin<'a> {
    Ignore,
    Inherit,
    Pipe(Option<&'a str>),
}

impl Default for ExecFileWithCwdOptions<'_> {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            preserve_output_on_error: true,
            cwd: None,
            env: &[],
            stdin: ExecFileStdin::Pipe(None),
        }
    }
}

/// Rust eager-Promise adapter for `execFileNoThrow(..., {input, useCwd:false})`.
/// Maps to: CC `utils/execFileNoThrow.ts:26-44,89-161`.
pub(crate) fn exec_file_no_throw_with_input(
    program: &str,
    args: &[&str],
    timeout: Duration,
    input: &str,
) -> impl std::future::Future<Output = ExecFileOutput> + Send + 'static {
    exec_file_no_throw_with_cwd_options(
        program,
        args,
        ExecFileWithCwdOptions {
            timeout,
            stdin: ExecFileStdin::Pipe(Some(input)),
            ..Default::default()
        },
    )
}

// Native node:path.resolve boundary selected by Execa normalizeCwd. This is
// lexical: resolving a symlink/.. via the OS would select a different cwd.
// Keep it local to the source's Execa options boundary; expand_path also
// expands ~, trims and normalizes Unicode, none of which apply here.
fn native_resolve_cwd(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            component => resolved.push(component.as_os_str()),
        }
    }
    Ok(resolved)
}

/// Native async/options carrier for the same source function; sync callers
/// retain their historical byte contract. The one StartedProcess runner
/// starts eagerly and outlives a dropped observer (PORTING.md A6/A7 and
/// existing clipboard eager process adapter; CC Promise ≙ owned worker/future).
/// Maps to: CC `utils/execFileNoThrow.ts#execFileNoThrowWithCwd:89-161`.
pub(crate) fn exec_file_no_throw_with_cwd_options(
    program: &str,
    args: &[&str],
    options: ExecFileWithCwdOptions<'_>,
) -> impl std::future::Future<Output = ExecFileOutput> + Send + 'static {
    let preserve = options.preserve_output_on_error;
    // Resolve once before both spawn and diagnostic construction, as Execa
    // normalizeCwd does before invoking the native child_process boundary.
    let (resolved_cwd, resolve_error) = match options.cwd.map(native_resolve_cwd).transpose() {
        Ok(cwd) => (cwd, None),
        Err(error) => (None, Some(error)),
    };
    let options = ExecFileWithCwdOptions {
        cwd: resolved_cwd.as_deref(),
        ..options
    };
    let inherited_env = crate::utils::process_env::snapshot();
    let started = match resolve_error {
        Some(error) => Err(error),
        None => StartedProcess::spawn_options(program, args, &options, &inherited_env),
    };
    let spawn_error = started.as_ref().err().map(|error| {
        let mut output = ExecFileOutput::failed();
        if preserve {
            let code = crate::utils::errors::io_errno_code(error).unwrap_or("EIO");
            let mut original = String::new();
            if let Some(cwd) = options.cwd {
                if let Err(cwd_error) = std::fs::metadata(cwd) {
                    original = format!(
                        "The \"cwd\" option is invalid: {}.\n{}\n",
                        cwd.display(),
                        crate::utils::errors::format_native_file_error(
                            &cwd_error,
                            "stat",
                            Some(cwd)
                        )
                    );
                }
            }
            original.push_str(&crate::utils::errors::format_native_file_error(
                error,
                "posix_spawn",
                Some(std::path::Path::new(program)),
            ));
            let message = format!(
                "Command failed with {code}: {}\n{original}",
                native_execa_command(program, args)
            );
            output.error = Some(get_error_message(Some(&message), None, 1));
        }
        // reject:false spawn failures enter .then(result.failed), not catch;
        // original logError is therefore intentionally not invoked here.
        output
    });
    let (sender, receiver) = futures::channel::oneshot::channel();
    match started {
        Ok(process) => {
            // Share only the pre-worker ownership handoff so failed thread
            // creation can still kill/reap the child. There is one runner.
            let pending = std::sync::Arc::new(std::sync::Mutex::new(Some(process)));
            let worker_pending = pending.clone();
            if std::thread::Builder::new()
                .name("exec-file-no-throw".to_owned())
                .spawn(move || {
                    let process = worker_pending.lock().unwrap().take().unwrap();
                    let mut output = process.finish(preserve);
                    // Execa's default stripFinalNewline removes one LF and
                    // its preceding CR, never arbitrary whitespace. Keep the
                    // legacy synchronous adapter's byte contract unchanged.
                    for stream in [&mut output.stdout, &mut output.stderr] {
                        if stream.ends_with('\n') {
                            stream.pop();
                            if stream.ends_with('\r') {
                                stream.pop();
                            }
                        }
                    }
                    let _ = sender.send(output);
                })
                .is_err()
            {
                if let Some(mut process) = pending.lock().unwrap().take() {
                    let _ = process.child.kill();
                    let _ = process.child.wait();
                }
            }
        }
        Err(_) => {
            let _ = sender.send(spawn_error.unwrap());
        }
    }
    async move { receiver.await.unwrap_or_else(|_| ExecFileOutput::failed()) }
}

// Rust process/pipe ownership carrier shared by the sync and eager adapters;
// this does not introduce a second source-level execution function.
struct StartedProcess {
    child: Child,
    input: Option<Vec<u8>>,
    started: Instant,
    timeout: Duration,
    command: String,
}

impl StartedProcess {
    fn spawn_options(
        program: &str,
        args: &[&str],
        options: &ExecFileWithCwdOptions<'_>,
        inherited_env: &crate::utils::process_env::EnvSnapshot,
    ) -> std::io::Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let input = match options.stdin {
            ExecFileStdin::Ignore => {
                command.stdin(Stdio::null());
                None
            }
            ExecFileStdin::Inherit => {
                command.stdin(Stdio::inherit());
                None
            }
            ExecFileStdin::Pipe(input) => {
                command.stdin(Stdio::piped());
                input.map(|input| input.as_bytes().to_vec())
            }
        };
        // CC passes env to Execa with extendEnv's true default. Snapshot the
        // established runtime process.env carrier before spawn, then overlay
        // own properties (including explicit undefined removals).
        command.env_clear().envs(inherited_env.iter());
        for (key, value) in options.env {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        if let Some(cwd) = options.cwd {
            command.current_dir(cwd);
        }
        let started = Instant::now();
        Ok(Self {
            child: command.spawn()?,
            input,
            started,
            timeout: options.timeout,
            command: native_execa_command(program, args),
        })
    }

    // Native Execa shortMessage boundary, separate from CC's getErrorMessage.
    // Verified Unix exit/timeout and TERM; KILL/INT strings follow the installed
    // Execa dependency but are not independently oracle-verified; other signals,
    // Windows spawn diagnostics and arbitrary I/O failures remain partial.
    fn native_error_message(
        &self,
        code: i32,
        signal: Option<i32>,
        timed_out: bool,
        forced: bool,
    ) -> Option<String> {
        let prefix = if timed_out {
            format!(
                "Command timed out after {} milliseconds{}",
                self.timeout.as_millis(),
                if forced {
                    " and was forcefully terminated after 5000 milliseconds"
                } else {
                    ""
                }
            )
        } else if let Some(signal) = signal {
            #[cfg(unix)]
            let description = match signal {
                libc::SIGTERM => "Command was killed with SIGTERM (Termination)".to_owned(),
                libc::SIGKILL => "Command was killed with SIGKILL (Forced termination)".to_owned(),
                libc::SIGINT => {
                    "Command was killed with SIGINT (User interruption with CTRL-C)".to_owned()
                }
                other => format!("Command was killed with signal {other}"),
            };
            #[cfg(not(unix))]
            let description = format!("Command was killed with signal {signal}");
            description
        } else if code != 0 {
            format!("Command failed with exit code {code}")
        } else {
            return None;
        };
        Some(get_error_message(
            Some(&format!("{prefix}: {}", self.command)),
            None,
            code,
        ))
    }

    #[cfg(unix)]
    fn finish(mut self, preserve_output_on_error: bool) -> ExecFileOutput {
        use std::os::fd::AsRawFd;

        // Execa drives all three pipes concurrently. Nonblocking owned pipe
        // descriptors let the same runner enforce timeout even when the child
        // never reads a clipboard payload larger than its stdin pipe buffer.
        fn nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
            let fd = pipe.as_raw_fd();
            // SAFETY: the descriptor is borrowed from an owned live pipe;
            // fcntl changes flags only and does not transfer ownership.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags == -1
                || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        fn read_available(reader: &mut Option<impl Read>, captured: &mut Vec<u8>) -> bool {
            let Some(pipe) = reader.as_mut() else {
                return false;
            };
            let mut buffer = [0u8; 16 * 1024];
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    *reader = None;
                    true
                }
                Ok(count) => {
                    captured.extend_from_slice(&buffer[..count]);
                    true
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    false
                }
                Err(_) => {
                    *reader = None;
                    true
                }
            }
        }

        let mut stdin = self.child.stdin.take();
        let mut stdout = self.child.stdout.take();
        let mut stderr = self.child.stderr.take();
        let pipe_setup = (|| {
            if let Some(pipe) = &stdin {
                nonblocking(pipe)?;
            }
            if let Some(pipe) = &stdout {
                nonblocking(pipe)?;
            }
            if let Some(pipe) = &stderr {
                nonblocking(pipe)?;
            }
            Ok::<(), std::io::Error>(())
        })();
        if pipe_setup.is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return ExecFileOutput::failed();
        }
        let close_input = self.input.is_some();
        let input = self.input.take().unwrap_or_default();
        let mut input_offset = 0;
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let mut status = None;
        let mut timed_out = false;
        let mut termination_started = None;
        let mut force_killed = false;
        loop {
            let mut progressed = false;
            if let Some(pipe) = stdin.as_mut().filter(|_| close_input) {
                if input_offset == input.len() {
                    // Drop closes stdin, delivering EOF before awaiting exit.
                    stdin = None;
                    progressed = true;
                } else {
                    match pipe
                        .write(&input[input_offset..input.len().min(input_offset + 16 * 1024)])
                    {
                        Ok(0) => {
                            stdin = None;
                        }
                        Ok(count) => {
                            input_offset += count;
                            progressed = true;
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => {
                            stdin = None;
                        }
                    }
                }
            }
            progressed |= read_available(&mut stdout, &mut stdout_bytes);
            progressed |= read_available(&mut stderr, &mut stderr_bytes);
            if status.is_none() {
                match self.child.try_wait() {
                    Ok(result) => status = result,
                    Err(_) => {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                }
            }
            if status.is_some() && stdout.is_none() && stderr.is_none() {
                break;
            }
            if !timed_out && !self.timeout.is_zero() && self.started.elapsed() >= self.timeout {
                timed_out = true;
                // Maps to: execFileNoThrow.ts:110-120 delegated Execa defaults:
                // terminate/timeout.js calls kill(); arguments/options.js
                // defaults to SIGTERM, then terminate/kill.js waits 5000 ms.
                // Do not replace graceful termination with immediate SIGKILL:
                // TERM handlers can perform effects and supply an exit code.
                if status.is_none() {
                    // SAFETY: child.id() identifies the owned, unreaped child.
                    let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
                    if sent == 0 {
                        termination_started = Some(Instant::now());
                    }
                }
            }
            if !force_killed
                && status.is_none()
                && termination_started
                    .is_some_and(|started| started.elapsed() >= Duration::from_secs(5))
            {
                let _ = self.child.kill();
                force_killed = true;
            }
            // The source waits for stdio EOF even if the direct child already
            // exited (resolve/wait-subprocess.js catch joins stdio promises).
            // Inherited descendant pipes may therefore extend completion past
            // timeout; keep polling owned nonblocking fds instead of blocking
            // in read_to_end/recv/join or inventing an early completion policy.
            if !progressed {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        // Source uses result.exitCode ?? 1 even when timedOut: a TERM
        // handler exiting 0 or 7 must keep that actual code.
        let code = status.and_then(|status| status.code()).unwrap_or(1);
        let mut output = captured_output(
            code,
            stdout_bytes,
            stderr_bytes,
            preserve_output_on_error,
            timed_out,
        );
        if preserve_output_on_error {
            use std::os::unix::process::ExitStatusExt;
            output.error = self.native_error_message(
                code,
                status.and_then(|status| status.signal()),
                timed_out,
                force_killed,
            );
        }
        output
    }

    #[cfg(not(unix))]
    fn finish(mut self, preserve_output_on_error: bool) -> ExecFileOutput {
        // Portable pipe ownership carrier: like Execa, wait for stdio EOF
        // after child exit, including pipes inherited by a descendant.
        // Unix uses the nonblocking branch above and retains no pipe threads.
        fn capture(reader: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut reader = reader;
                let mut bytes = Vec::new();
                let _ = reader.read_to_end(&mut bytes);
                let _ = sender.send(bytes);
            });
            receiver
        }
        let stdout = self.child.stdout.take().map(capture);
        let stderr = self.child.stderr.take().map(capture);
        if let (Some(mut stdin), Some(input)) = (self.child.stdin.take(), self.input.take()) {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
        }
        let mut timed_out = false;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if !self.timeout.is_zero() && self.started.elapsed() >= self.timeout => {
                    timed_out = true;
                    let _ = self.child.kill();
                    break self.child.wait().ok();
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
            }
        };
        let receive = |receiver: Option<std::sync::mpsc::Receiver<Vec<u8>>>| {
            receiver
                .and_then(|receiver| receiver.recv().ok())
                .unwrap_or_default()
        };
        let stdout = receive(stdout);
        let stderr = receive(stderr);
        let code = status.and_then(|status| status.code()).unwrap_or(1);
        let mut output = captured_output(code, stdout, stderr, preserve_output_on_error, timed_out);
        if preserve_output_on_error {
            output.error = self.native_error_message(code, None, timed_out, false);
        }
        output
    }
}

// Rust result conversion shared by native pipe backends; preserves the
// existing sync consumers' output bytes (including their trailing newline).
fn captured_output(
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    preserve_output_on_error: bool,
    failed_by_timeout: bool,
) -> ExecFileOutput {
    let preserve_output = (code == 0 && !failed_by_timeout) || preserve_output_on_error;
    ExecFileOutput {
        code,
        stdout: if preserve_output {
            String::from_utf8_lossy(&stdout).into_owned()
        } else {
            String::new()
        },
        stderr: if preserve_output {
            String::from_utf8_lossy(&stderr).into_owned()
        } else {
            String::new()
        },
        error: None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, in_child_process};

    fn workdir() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("exec-file-no-throw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }

    /// CC osc.ts:96-101,138-157: the framework service uses this real eager
    /// executor, inherits cwd, and returns DCS only after load-buffer succeeds.
    #[test]
    fn clipboard_backend_matches_official_eager_tmux_and_failure_fallback() {
        use std::os::unix::fs::PermissionsExt;
        // iocraft's clipboard reads the session's terminal from the OS environment.
        let terminal = [
            ("SSH_CONNECTION", Some("fixture")),
            ("TMUX", Some("fixture")),
            ("LC_TERMINAL", Some("iTerm2")),
        ];
        if !in_child_process(
            module_path!(),
            "clipboard_backend_matches_official_eager_tmux_and_failure_fallback",
            &terminal,
        ) {
            return;
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = workdir();
        let executable = dir.join("tmux");
        std::fs::write(&executable, "#!/bin/sh\n/bin/cat > \"$CLIP_EXEC_FIXTURE/input\"\nprintf '%s\\n' \"$@\" > \"$CLIP_EXEC_FIXTURE/args\"\n/bin/pwd > \"$CLIP_EXEC_FIXTURE/cwd\"\n[ -f \"$CLIP_EXEC_FIXTURE/success\" ]\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _path = EnvVarGuard::set("PATH", &dir);
        let _fixture = EnvVarGuard::set("CLIP_EXEC_FIXTURE", &dir);
        let clipboard = iocraft::Clipboard::new(std::sync::Arc::new(ExecFileClipboardBackend));
        let future = clipboard.set_clipboard("中文\n🌈");
        let deadline = Instant::now() + Duration::from_secs(4);
        while !dir.join("args").exists() {
            assert!(
                Instant::now() < deadline,
                "setClipboard must start tmux before its future is polled"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let raw = futures::executor::block_on(future);
        let terminator = if iocraft::ClipboardBackend::is_kitty(&ExecFileClipboardBackend) {
            "\x1b\\"
        } else {
            "\x07"
        };
        assert_eq!(raw, format!("\x1b]52;c;5Lit5paHCvCfjIg={terminator}"));
        assert_eq!(
            std::fs::read_to_string(dir.join("input")).unwrap(),
            "中文\n🌈"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("args")).unwrap(),
            "load-buffer\n-\n"
        );
        assert_eq!(
            std::path::Path::new(std::fs::read_to_string(dir.join("cwd")).unwrap().trim())
                .canonicalize()
                .unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        std::fs::write(dir.join("success"), "").unwrap();
        assert_eq!(
            futures::executor::block_on(clipboard.set_clipboard("A")),
            "\x1bPtmux;\x1b\x1b]52;c;QQ==\x07\x1b\\"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:108-120`; Promise executor eagerly
    /// starts execa before the returned Promise is consumed.
    #[test]
    fn input_process_launches_without_poll_and_survives_dropped_future() {
        let dir = workdir();
        let marker = dir.join("copied");
        let future = exec_file_no_throw_with_input(
            "/bin/sh",
            &["-c", "cat > \"$1\"", "sh", marker.to_str().unwrap()],
            Duration::from_secs(2),
            "日本語\nclipboard",
        );
        drop(future);
        let started = Instant::now();
        while std::fs::read_to_string(&marker).ok().as_deref() != Some("日本語\nclipboard") {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "unpolled/dropped future must still write clipboard input"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:119,137-142`; EOF is part of input
    /// delivery, so programs waiting for EOF must finish before the result.
    #[test]
    fn input_closes_stdin_and_captures_both_streams_before_resolving() {
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/sh",
            &["-c", "cat; printf done >&2"],
            Duration::from_secs(2),
            "α\nβ\n",
        ));
        assert_eq!(result.code, 0);
        assert_eq!(result.stdout, "α\nβ");
        assert_eq!(result.stderr, "done");
        let empty = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/cat",
            &[],
            Duration::from_secs(2),
            "",
        ));
        assert_eq!(empty.code, 0);
        assert!(empty.stdout.is_empty());
    }

    #[test]
    fn large_input_is_drained_concurrently_with_captured_output() {
        let input = "日本語 clipboard\n".repeat(100_000);
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/cat",
            &[],
            Duration::from_secs(5),
            &input,
        ));
        assert_eq!(result.code, 0);
        assert_eq!(result.stdout, input.strip_suffix('\n').unwrap());
        assert!(result.stderr.is_empty());
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:113,119`; timeout covers a blocked
    /// input pipe as well as process waiting. Payload deliberately exceeds it.
    #[test]
    fn input_timeout_does_not_wait_for_a_child_that_never_reads() {
        let input = "x".repeat(2 * 1024 * 1024);
        let started = Instant::now();
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/sh",
            &["-c", "exec sleep 5"],
            Duration::from_millis(100),
            &input,
        ));
        assert_eq!(result.code, 1);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn inherited_output_pipes_match_official_completion_after_direct_child_exit() {
        // Real Bun oracle: timeout100 ms still waits ~1010 ms for this
        // descendant's EOF and returns the direct child's actual code 0.
        let started = Instant::now();
        let result = exec_file_no_throw(
            "/bin/sh",
            &["-c", "sleep 1 & printf partial"],
            Duration::from_millis(100),
        );
        assert_eq!(result.code, 0);
        assert_eq!(result.stdout, "partial");
        assert_eq!(
            result.error.as_deref(),
            Some("Command timed out after 100 milliseconds: /bin/sh -c 'sleep 1 & printf partial'")
        );
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn timeout_uses_sigterm_and_preserves_handler_effects_and_exit_code() {
        for code in [0, 7] {
            let script = format!(
                "trap 'printf TERM_SEEN; exit {code}' TERM; for tick in 1 2 3 4 5 6 7 8 9 10; do sleep 0.1; done"
            );
            let started = Instant::now();
            let result = futures::executor::block_on(exec_file_no_throw_with_input(
                "/bin/sh",
                &["-c", &script],
                Duration::from_millis(100),
                "",
            ));
            assert_eq!(result.code, code);
            assert_eq!(result.stdout, "TERM_SEEN");
            assert!(started.elapsed() >= Duration::from_millis(100));
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn ignored_sigterm_uses_official_five_second_force_kill_delay() {
        let started = Instant::now();
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/sh",
            &["-c", "trap '' TERM; exec sleep 20"],
            Duration::from_millis(100),
            "",
        ));
        assert_eq!(result.code, 1);
        assert!(started.elapsed() >= Duration::from_millis(5100));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn timeout_failure_suppresses_output_even_when_term_handler_exits_zero() {
        let result = exec_file_no_throw_with_cwd(
            "/bin/sh",
            &[
                "-c",
                "trap 'printf TERM_SEEN; exit 0' TERM; for tick in 1 2 3 4 5 6 7 8 9 10; do sleep 0.1; done",
            ],
            Duration::from_millis(100),
            None,
            false,
        );
        assert_eq!(result.code, 0);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn input_output_strips_only_one_final_newline_like_execa() {
        for (input, expected) in [
            ("a\r\n", "a"),
            ("a\n\n", "a\n"),
            ("a\r", "a\r"),
            ("a ", "a "),
        ] {
            let result = futures::executor::block_on(exec_file_no_throw_with_input(
                "/bin/cat",
                &[],
                Duration::from_secs(2),
                input,
            ));
            assert_eq!(result.code, 0);
            assert_eq!(result.stdout, expected);
        }
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:39,96,124-147`.
    #[test]
    fn input_inherits_process_cwd_and_never_throws_on_failure() {
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/pwd",
            &[],
            Duration::from_secs(2),
            "",
        ));
        assert_eq!(result.code, 0);
        assert_eq!(
            std::path::Path::new(result.stdout.trim_end())
                .canonicalize()
                .unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/bin/sh",
            &["-c", "printf out; printf err >&2; exit 7"],
            Duration::from_secs(2),
            "",
        ));
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr.as_str()),
            (7, "out", "err")
        );
        let result = futures::executor::block_on(exec_file_no_throw_with_input(
            "/no-such-exec-file-no-throw",
            &[],
            Duration::from_secs(2),
            "",
        ));
        assert_eq!(result.code, 1);
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
    }

    #[test]
    fn existing_sync_cwd_and_output_preservation_are_unchanged() {
        let dir = workdir();
        let result =
            exec_file_no_throw_with_cwd("/bin/pwd", &[], Duration::from_secs(2), Some(&dir), true);
        assert_eq!(result.code, 0);
        assert_eq!(
            std::path::Path::new(result.stdout.trim_end())
                .canonicalize()
                .unwrap(),
            dir.canonicalize().unwrap()
        );
        for preserve in [true, false] {
            let result = exec_file_no_throw_with_cwd(
                "/bin/sh",
                &["-c", "printf 'out\\n'; printf 'err\\n' >&2; exit 3"],
                Duration::from_secs(2),
                None,
                preserve,
            );
            assert_eq!(result.code, 3);
            assert_eq!(result.stdout, if preserve { "out\n" } else { "" });
            assert_eq!(result.stderr, if preserve { "err\n" } else { "" });
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Maps to: CC `utils/execFileNoThrow.ts#getErrorMessage:73-85`;
    /// branch-oracle.json invokes the
    /// original wrapper with controlled Execa results, including empty signal.
    #[test]
    fn get_error_message_matches_official_priority_and_empty_signal() {
        assert_eq!(
            get_error_message(Some("short"), Some("SIGTERM"), 7),
            "short"
        );
        assert_eq!(get_error_message(Some(""), Some("SIGTERM"), 7), "SIGTERM");
        assert_eq!(get_error_message(None, Some(""), 7), "");
        assert_eq!(get_error_message(None, None, 7), "7");
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:110-120`.
    /// Original Bun + Execa oracle: real process.env inheritance, own
    /// overrides and undefined removal, with final env captured before await.
    #[test]
    fn options_matches_official_env_inheritance_override_deletion_and_cwd() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _inherit = EnvVarGuard::set("EXEC_PARITY_INHERIT", "inherited");
        let _override = EnvVarGuard::set("EXEC_PARITY_OVERRIDE", "parent");
        let _remove = EnvVarGuard::set("EXEC_PARITY_REMOVE", "delete-me");
        let dir = workdir();
        let env = [
            ("EXEC_PARITY_OVERRIDE".into(), Some("child".into())),
            ("EXEC_PARITY_ADDED".into(), Some("new".into())),
            ("EXEC_PARITY_REMOVE".into(), None),
        ];
        let future = exec_file_no_throw_with_cwd_options(
            "/bin/sh",
            &[
                "-c",
                "printf '%s|%s|%s|%s\\n' \"$EXEC_PARITY_INHERIT\" \"$EXEC_PARITY_OVERRIDE\" \"$EXEC_PARITY_ADDED\" \"${EXEC_PARITY_REMOVE-unset}\"; /bin/pwd",
            ],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                cwd: Some(&dir),
                env: &env,
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        );
        crate::utils::process_env::set("EXEC_PARITY_INHERIT", "after-spawn");
        let result = futures::executor::block_on(future);
        assert_eq!(result.code, 0);
        assert!(result.error.is_none());
        let (values, cwd) = result.stdout.split_once('\n').unwrap();
        assert_eq!(values, "inherited|child|new|unset");
        assert_eq!(
            std::path::Path::new(cwd).canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:110-135`.
    /// Native oracle distinguishes ignore EOF from an open
    /// default/explicit pipe without input. Do not turn None into empty input.
    #[test]
    fn options_matches_official_stdin_ignore_and_open_pipe() {
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/cat",
            &[],
            ExecFileWithCwdOptions {
                timeout: Duration::from_millis(200),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        ));
        assert_eq!(result.code, 0);
        assert!(result.error.is_none());
        assert!(result.stdout.is_empty());
        let started = Instant::now();
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/cat",
            &[],
            ExecFileWithCwdOptions {
                timeout: Duration::from_millis(200),
                ..Default::default()
            },
        ));
        assert_eq!(result.code, 1);
        assert_eq!(
            result.error.as_deref(),
            Some("Command timed out after 200 milliseconds: /bin/cat")
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:123-154`.
    /// Real original wrapper/execa9.6.1: nonzero is .then(result.failed),
    /// preserve false omits error, and one terminal newline is stripped.
    #[test]
    fn options_matches_official_nonzero_output_and_short_message() {
        for preserve in [true, false] {
            let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
                "/bin/sh",
                &["-c", "printf out; printf err >&2; exit 7"],
                ExecFileWithCwdOptions {
                    timeout: Duration::from_secs(2),
                    preserve_output_on_error: preserve,
                    stdin: ExecFileStdin::Ignore,
                    ..Default::default()
                },
            ));
            assert_eq!(result.code, 7);
            assert_eq!(result.stdout, if preserve { "out" } else { "" });
            assert_eq!(result.stderr, if preserve { "err" } else { "" });
            assert_eq!(
                result.error.as_deref(),
                if preserve {
                    Some(
                        "Command failed with exit code 7: /bin/sh -c 'printf out; printf err >&2; exit 7'",
                    )
                } else {
                    None
                }
            );
        }
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/sh",
            &["-c", "printf 'a\\n\\n'; printf 'b\\r\\n' >&2"],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        ));
        assert_eq!(
            (result.stdout.as_str(), result.stderr.as_str(), result.code),
            ("a\n", "b", 0)
        );
        assert!(result.error.is_none());
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:123-158`.
    /// Source ENOENT is a failed result, not catch/logError; original Bun
    /// native spawn message includes the program, and bad cwd adds stat first.
    #[test]
    fn options_matches_official_spawn_failure_and_missing_cwd() {
        let dir = workdir();
        let missing = dir.join("missing");
        for preserve in [true, false] {
            let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
                missing.to_str().unwrap(),
                &[],
                ExecFileWithCwdOptions {
                    timeout: Duration::from_secs(2),
                    preserve_output_on_error: preserve,
                    ..Default::default()
                },
            ));
            assert_eq!(result.code, 1);
            assert!(result.stdout.is_empty() && result.stderr.is_empty());
            assert_eq!(result.error, preserve.then(|| format!("Command failed with ENOENT: {}\nENOENT: no such file or directory, posix_spawn '{}'", missing.display(), missing.display())));
        }
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/pwd",
            &[],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                cwd: Some(&missing),
                ..Default::default()
            },
        ));
        assert_eq!(
            result.error,
            Some(format!(
                "Command failed with ENOENT: /bin/pwd\nThe \"cwd\" option is invalid: {}.\nENOENT: no such file or directory, stat '{}'\nENOENT: no such file or directory, posix_spawn '/bin/pwd'",
                missing.display(),
                missing.display()
            ))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:123-146`.
    /// Source result.failed remains true for timed-out TERM handlers that
    /// return zero. Preserve false omits output and error but retains code 0.
    #[test]
    fn options_matches_official_timeout_zero_exit_and_signal_diagnostics() {
        let script = "trap \"printf caught; exit 0\" TERM; printf ready; while :; do :; done";
        for preserve in [true, false] {
            let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
                "/bin/sh",
                &["-c", script],
                ExecFileWithCwdOptions {
                    timeout: Duration::from_millis(150),
                    preserve_output_on_error: preserve,
                    stdin: ExecFileStdin::Ignore,
                    ..Default::default()
                },
            ));
            assert_eq!(result.code, 0);
            assert_eq!(result.stdout, if preserve { "readycaught" } else { "" });
            assert_eq!(
                result.error.as_deref(),
                if preserve {
                    Some(
                        "Command timed out after 150 milliseconds: /bin/sh -c 'trap \"printf caught; exit 0\" TERM; printf ready; while :; do :; done'",
                    )
                } else {
                    None
                }
            );
        }
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/sh",
            &["-c", "printf ready; kill -TERM $$"],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        ));
        assert_eq!(result.code, 1);
        assert_eq!(result.stdout, "ready");
        assert_eq!(
            result.error.as_deref(),
            Some(
                "Command was killed with SIGTERM (Termination): /bin/sh -c 'printf ready; kill -TERM $$'"
            )
        );
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:128-140`, delegated Execa
    /// escapedCommand; this is a native diagnostic boundary assertion.
    #[test]
    fn options_matches_official_argv_diagnostic_escaping() {
        assert_eq!(
            native_execa_command(
                "git",
                &[
                    "clone",
                    "https://github.com/o/r.git",
                    "--branch=main",
                    "a'b",
                    "a\nb",
                    ""
                ]
            ),
            "git clone 'https://github.com/o/r.git' '--branch=main' 'a'\\''b' 'a\\nb' ''"
        );
    }

    /// Maps to: CC `utils/execFileNoThrow.ts:110-116`, delegated Execa
    /// normalizeCwd/node:path.resolve; review-a-oracle.json is a real Bun run.
    #[test]
    fn options_matches_official_lexical_symlink_parent_and_relative_missing_cwd() {
        let dir = workdir();
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir_all(b.join("inner")).unwrap();
        std::os::unix::fs::symlink(b.join("inner"), a.join("link")).unwrap();
        let cwd = a.join("link/..");
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/pwd",
            &[],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                cwd: Some(&cwd),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        ));
        assert_eq!(result.code, 0);
        assert!(result.error.is_none());
        assert_eq!(
            std::path::Path::new(&result.stdout).canonicalize().unwrap(),
            a.canonicalize().unwrap()
        );
        // The old synchronous OS-cwd adapter deliberately remains separate.
        let legacy =
            exec_file_no_throw_with_cwd("/bin/pwd", &[], Duration::from_secs(2), Some(&cwd), true);
        assert_eq!(
            std::path::Path::new(legacy.stdout.trim_end())
                .canonicalize()
                .unwrap(),
            b.canonicalize().unwrap()
        );
        let relative =
            std::path::PathBuf::from(format!("./missing-exec-cwd-{}", uuid::Uuid::new_v4()));
        let absolute = std::env::current_dir()
            .unwrap()
            .join(relative.file_name().unwrap());
        let result = futures::executor::block_on(exec_file_no_throw_with_cwd_options(
            "/bin/pwd",
            &[],
            ExecFileWithCwdOptions {
                timeout: Duration::from_secs(2),
                cwd: Some(&relative),
                ..Default::default()
            },
        ));
        assert_eq!(
            result.error,
            Some(format!(
                "Command failed with ENOENT: /bin/pwd\nThe \"cwd\" option is invalid: {}.\nENOENT: no such file or directory, stat '{}'\nENOENT: no such file or directory, posix_spawn '/bin/pwd'",
                absolute.display(),
                absolute.display()
            ))
        );
        assert_eq!(
            native_resolve_cwd(std::path::Path::new("/../../a//./e\u{301}/../ spaced /~")).unwrap(),
            std::path::PathBuf::from("/a/ spaced /~")
        );
        assert_eq!(
            native_resolve_cwd(std::path::Path::new("/a/e\u{301}")).unwrap(),
            std::path::PathBuf::from("/a/e\u{301}")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
