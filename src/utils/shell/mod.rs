//! Shell selection and process spawn.
//!
//! Maps to: CC `utils/Shell.ts:1-477`.
//!
//! Rust module-path L1: TypeScript has both case-sensitive `utils/Shell.ts`
//! and the `utils/shell/` directory. Rust cannot declare `shell.rs` beside a
//! `shell/` module, so this `shell/mod.rs` owns `Shell.ts` while its child files
//! retain `utils/shell/*.ts` ownership.

pub mod bash_provider;
pub mod output_limits;
pub mod powershell_detection;
pub mod read_only_command_validation;
pub mod shell_provider;
pub mod shell_tool_utils;

use crate::utils::process_env::JsTruthy;
use shell_provider::{ShellProvider, ShellType};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

static BASH_PROVIDER: LazyLock<Mutex<Option<Arc<bash_provider::BashShellProvider>>>> =
    LazyLock::new(|| Mutex::new(None));
static SHELL_CLEANUP_REGISTRATION: LazyLock<crate::utils::cleanup_registry::CleanupRegistration> =
    LazyLock::new(|| {
        crate::utils::cleanup_registry::register_cleanup(|| async {
            crate::tasks::local_shell_task::kill_shell_tasks::kill_all_shell_tasks();
            cleanup_shell_config();
        })
    });

fn is_executable(shell_path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if shell_path
            .metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
        {
            return true;
        }
    }
    #[cfg(windows)]
    if shell_path.is_file() {
        return true;
    }
    let mut command = std::process::Command::new(shell_path);
    // CC `Shell.ts` execFileSync inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            let start = std::time::Instant::now();
            loop {
                if let Some(status) = child.try_wait()? {
                    return Ok(status.success());
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .unwrap_or(false)
}

/// CC `Shell.ts:99` `which`: Bun.which reads the startup PATH, as
/// `utils/which.rs` does.
fn which(executable: &str) -> Option<PathBuf> {
    let path = crate::utils::process_env::startup_snapshot()
        .var_os("PATH")?
        .to_os_string();
    std::env::split_paths(&path)
        .map(|directory| {
            if cfg!(windows) {
                directory.join(format!("{executable}.exe"))
            } else {
                directory.join(executable)
            }
        })
        .find(|candidate| is_executable(candidate))
}

/// Maps to CC `findSuitableShell()`.
pub fn find_suitable_shell() -> Result<PathBuf, String> {
    if cfg!(windows) {
        return crate::utils::windows_paths::find_git_bash_path();
    }
    if let Some(override_path) = crate::utils::process_env::var_os("CLAUDE_CODE_SHELL")
        .as_deref()
        .truthy()
        .map(PathBuf::from)
    {
        let text = override_path.display().to_string();
        if (text.contains("bash") || text.contains("zsh")) && is_executable(&override_path) {
            return Ok(override_path);
        }
    }

    let env_shell = crate::utils::process_env::var_os("SHELL").map(PathBuf::from);
    let prefer_bash = env_shell
        .as_ref()
        .is_some_and(|path| path.display().to_string().contains("bash"));
    if let Some(shell) = env_shell.as_ref().filter(|path| {
        let text = path.display().to_string();
        (text.contains("bash") || text.contains("zsh")) && is_executable(path)
    }) {
        return Ok(shell.clone());
    }

    let order = if prefer_bash {
        ["bash", "zsh"]
    } else {
        ["zsh", "bash"]
    };
    for name in order {
        if let Some(path) = which(name) {
            return Ok(path);
        }
        for root in ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
            let path = Path::new(root).join(name);
            if is_executable(&path) {
                return Ok(path);
            }
        }
    }
    Err("No suitable shell found. Claude CLI requires a Posix shell environment. Please ensure you have a valid shell installed and the SHELL environment variable set.".to_string())
}

/// Maps to CC memoized `getShellConfig()`.
pub fn get_shell_config() -> Result<Arc<bash_provider::BashShellProvider>, String> {
    LazyLock::force(&SHELL_CLEANUP_REGISTRATION);
    let mut provider = BASH_PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(provider) = provider.as_ref() {
        return Ok(Arc::clone(provider));
    }
    let shell = find_suitable_shell()?;
    let created = Arc::new(bash_provider::create_bash_shell_provider(shell, false));
    *provider = Some(Arc::clone(&created));
    Ok(created)
}

/// Run the shell snapshot's graceful-shutdown cleanup.
pub fn cleanup_shell_config() {
    if let Ok(mut provider) = BASH_PROVIDER.lock() {
        if let Some(provider) = provider.take() {
            provider.cleanup_snapshot();
        }
    }
}

#[cfg(test)]
pub fn reset_shell_config_for_test() {
    cleanup_shell_config();
}

#[derive(Default)]
pub struct ExecOptions {
    pub timeout: Option<Duration>,
    pub cwd: Option<PathBuf>,
    pub prevent_cwd_changes: bool,
    pub should_use_sandbox: bool,
    pub should_auto_background: bool,
}

fn open_output_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .write(true)
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    options.write(true).create(true).truncate(true);
    options.open(path)
}

/// Maps to CC `exec(command, abortSignal, shellType, options)`.
pub fn exec(
    command: &str,
    abort_signal: &crate::tool::AbortController,
    shell_type: ShellType,
    options: ExecOptions,
) -> Result<crate::utils::shell_command::ShellCommand, String> {
    if shell_type != ShellType::Bash {
        return Err("PowerShell provider must be resolved by powershellProvider".to_string());
    }
    let provider = get_shell_config()?;
    let id = &uuid::Uuid::new_v4().simple().to_string()[..4];
    // `Shell.ts:204-207`: `posixJoin(CLAUDE_CODE_TMPDIR || '/tmp',
    // getClaudeTempDirName())`, not `getClaudeTempDir()`: no symlink
    // resolution, so on macOS this is `/tmp/claude-<uid>`. Sandboxing runs on
    // macOS and Linux only, where `node:path.join` is the POSIX one.
    let sandbox_tmp_dir = crate::utils::fs_operations::native::join_path(
        &crate::utils::process_env::var_os("CLAUDE_CODE_TMPDIR")
            .as_deref()
            .truthy()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp")),
        Path::new(&crate::utils::permissions::filesystem::get_claude_temp_dir_name()),
    );
    let built = provider
        .build_exec_command(
            command,
            id,
            options
                .should_use_sandbox
                .then_some(sandbox_tmp_dir.as_path()),
            options.should_use_sandbox,
        )
        .map_err(|error| error.to_string())?;

    let mut cwd = options
        .cwd
        .unwrap_or_else(crate::bootstrap::state::get_original_cwd);
    if cwd.canonicalize().is_err() {
        let fallback = crate::bootstrap::state::get_original_cwd();
        if fallback.canonicalize().is_ok() {
            cwd = fallback;
        } else {
            return Ok(crate::utils::shell_command::create_failed_command(format!(
                "Working directory \"{}\" no longer exists. Please restart Claude from an existing directory.",
                cwd.display()
            )));
        }
    }
    if abort_signal.is_aborted() {
        return Ok(crate::utils::shell_command::create_aborted_command(
            None, None, None,
        ));
    }

    let (program, args, sandbox_cleanup_paths) = if options.should_use_sandbox {
        // `Shell.ts:266-272`: create sandbox temp directory for sandboxed
        // processes with secure permissions; a failure is logged, not fatal.
        // CC creates it after wrapping the command; here before, as bwrap
        // binds only writable paths that exist.
        if let Err(error) = crate::utils::fs_operations::get_fs_implementation()
            .mkdir_sync(&sandbox_tmp_dir, Some(0o700))
        {
            crate::utils::debug::log_for_debugging(&format!(
                "Failed to create {} directory: {error}",
                sandbox_tmp_dir.display()
            ));
        }
        // Cometix forward-port of 2.1.285 (user, 10-03): its sandbox temp
        // directory is the same per-uid root `/copy` uses, and is refused
        // unless it is the user's own; 2.1.88 uses one another user created.
        #[cfg(unix)]
        crate::utils::permissions::filesystem::verify_owned_temp_dir(&sandbox_tmp_dir)
            .map_err(|error| error.to_string())?;
        let settings = crate::utils::settings::get_initial_settings();
        let wrapped = crate::utils::sandbox::sandbox_adapter::wrap_shell_command(
            &provider.shell_path().display().to_string(),
            &built.command_string,
            &cwd,
            &settings,
        )
        .map_err(|error| format!("Sandbox unavailable: {error}"))?;
        (wrapped.program, wrapped.args, wrapped.cleanup_paths)
    } else {
        (
            provider.shell_path().display().to_string(),
            provider.get_spawn_args(&built.command_string),
            Vec::new(),
        )
    };

    let task_id = crate::task::generate_task_id(crate::task::TaskType::LocalBash);
    let file_mode = crate::utils::session_storage::is_session_write_enabled();
    let task_output = crate::utils::task::task_output::TaskOutput::new(&task_id, file_mode);
    let mut process = std::process::Command::new(&program);
    // User-authorized L2: unlike CC Shell.ts:330-332's open stdin pipe,
    // give non-interactive Bash an EOF source. The eval trailer redirects only
    // the final command of an &&/; list; earlier readers (including ls=eza)
    // would otherwise wait forever. Explicit redirects, pipelines and heredocs
    // still supply their own input. Hook payload pipes are owned separately.
    process
        .args(&args)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null());
    if file_mode {
        std::fs::create_dir_all(crate::utils::task::disk_output::get_task_output_dir())
            .map_err(|error| error.to_string())?;
        let output = open_output_file(task_output.path()).map_err(|error| error.to_string())?;
        let error_output = output.try_clone().map_err(|error| error.to_string())?;
        process
            .stdout(std::process::Stdio::from(output))
            .stderr(std::process::Stdio::from(error_output));
    } else {
        process
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }
    // Env layering matches CC `Shell.ts:317-328` spread order exactly:
    // `{ ...subprocessEnv(), SHELL, GIT_EDITOR, CLAUDECODE, ...envOverrides,
    // ...(ant ? { CLAUDE_CODE_SESSION_ID } : {}) }` — explicit entries are
    // applied after the scrubbed base so they survive the scrub.
    crate::utils::subprocess_env::apply_subprocess_env_std(&mut process);
    process
        .env("SHELL", provider.shell_path())
        .env("GIT_EDITOR", "true")
        .env("CLAUDECODE", "1");
    for (key, value) in provider.get_environment_overrides(command) {
        process.env(key, value);
    }
    if crate::utils::build_profile::build_audience().is_internal() {
        process.env(
            "CLAUDE_CODE_SESSION_ID",
            crate::bootstrap::state::get_session_id(),
        );
    }
    #[cfg(unix)]
    if provider.detached() {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x0800_0000);
    }

    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            task_output.delete_output_file();
            return Ok(crate::utils::shell_command::create_aborted_command(
                None,
                Some(error.to_string()),
                Some(126),
            ));
        }
    };

    let cwd_file = built.cwd_file_path;
    let prevent_cwd_changes = options.prevent_cwd_changes;
    let use_sandbox = options.should_use_sandbox;
    let finalizer: crate::utils::shell_command::ResultFinalizer = Box::new(move |result| {
        if use_sandbox {
            crate::utils::sandbox::sandbox_adapter::cleanup_after_command(&sandbox_cleanup_paths);
        }
        if !prevent_cwd_changes && result.background_task_id.is_none() {
            if let Ok(value) = std::fs::read_to_string(&cwd_file) {
                let value = value.trim();
                if !value.is_empty() {
                    let path = if cfg!(windows) {
                        PathBuf::from(crate::utils::windows_paths::posix_path_to_windows_path(
                            value,
                        ))
                    } else {
                        PathBuf::from(value)
                    };
                    result.cwd_after = Some(path);
                }
            }
        }
        let _ = std::fs::remove_file(&cwd_file);
    });

    Ok(crate::utils::shell_command::wrap_spawn(
        child,
        abort_signal.clone(),
        options.timeout.unwrap_or(DEFAULT_TIMEOUT),
        task_output,
        options.should_auto_background,
        Some(finalizer),
    ))
}

/// Set/resolve a physical cwd. Maps to CC `setCwd(path, relativeTo)`; Rust
/// returns the path so the query-owned ToolUseContext can commit it without a
/// process-global `chdir`, preserving concurrent-agent cwd isolation.
pub fn set_cwd(path: &Path, relative_to: Option<&Path>) -> Result<PathBuf, String> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        relative_to.unwrap_or_else(|| Path::new(".")).join(path)
    };
    resolved.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("Path \"{}\" does not exist", resolved.display())
        } else {
            error.to_string()
        }
    })
}

/// Transitional adapter for the not-yet-refactored PowerShell owner. Bash uses
/// the source-shaped `exec → ShellCommand` path above.
pub struct StreamedCommandRun {
    pub output: std::process::Output,
    pub timed_out: bool,
    pub aborted: bool,
}

pub fn run_command_streaming(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
    abort: &crate::tool::AbortController,
    on_output: Option<&mut dyn FnMut(&str)>,
) -> std::io::Result<StreamedCommandRun> {
    let task_output = crate::utils::task::task_output::TaskOutput::new(
        crate::task::generate_task_id(crate::task::TaskType::LocalBash),
        false,
    );
    let mut process = std::process::Command::new(program);
    process
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    // The PowerShell branch of CC `Shell.ts:317-328`'s env. `SHELL: undefined`
    // removes the key (Node skips undefined values). `envOverrides` would come
    // from `powershellProvider.getEnvironmentOverrides`, which is not ported.
    crate::utils::subprocess_env::apply_subprocess_env_std(&mut process);
    process
        .env_remove("SHELL")
        .env("GIT_EDITOR", "true")
        .env("CLAUDECODE", "1");
    if crate::utils::build_profile::build_audience().is_internal() {
        process.env(
            "CLAUDE_CODE_SESSION_ID",
            crate::bootstrap::state::get_session_id(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let shell = crate::utils::shell_command::wrap_spawn(
        process.spawn()?,
        abort.clone(),
        timeout,
        task_output,
        false,
        None,
    );
    let result = shell.wait_result();
    if let Some(on_output) = on_output {
        on_output(&result.stdout);
    }
    Ok(StreamedCommandRun {
        output: std::process::Output {
            status: exit_status_from_code(result.code),
            stdout: result.stdout.into_bytes(),
            stderr: result.stderr.into_bytes(),
        },
        timed_out: result.code == 143,
        aborted: result.interrupted,
    })
}

#[cfg(unix)]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code as u32)
}

/// Commit Shell.ts's completed `pwd -P` trailer into the invocation context,
/// invalidate session exports, and fire CwdChanged hooks without delaying the
/// tool result. Maps to CC `utils/Shell.ts:404-410`.
pub fn apply_cwd_after_command(context: &mut crate::tool::ToolUseContext, new_cwd: PathBuf) {
    use unicode_normalization::UnicodeNormalization as _;

    let old_cwd = context.effective_cwd();
    // CC normalizes `pwd -P` to NFC before comparing because APFS can return
    // an NFD spelling for the same physical directory.
    let same_physical_spelling = match (old_cwd.to_str(), new_cwd.to_str()) {
        (Some(old), Some(new)) => old.nfc().eq(new.nfc()),
        _ => old_cwd == new_cwd,
    };
    if same_physical_spelling {
        return;
    }
    context.cwd_override = Some(new_cwd.clone());
    crate::utils::session_environment::invalidate_session_env_cache();
    let run_hooks = move || async move {
        let _ = crate::services::hooks::env::execute_cwd_changed_hooks(
            &old_cwd.display().to_string(),
            &new_cwd.display().to_string(),
        )
        .await;
    };
    // Maps to CC `utils/Shell.ts:409` `void onCwdChangedForHooks(cwd, newCwd)`
    // — detached onto Node's ONE process event loop, so the hook run's lifetime
    // is the process, not the tool turn (PORTING.md § "Node-async → tokio"
    // A3). This owner is reached from inside tool execution, where the ambient
    // runtime is the turn's private per-query `current_thread` runtime
    // (`query.rs#spawn_query`); spawning there ties the hook run to the turn
    // and a slow hook is silently killed when the turn resolves.
    if let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
        runtime.spawn(run_hooks());
    } else {
        // Bash prompt-shell and direct SDK adapters can call this synchronous
        // owner with no published process runtime (and no multi-thread ambient
        // one). CC still fires CwdChanged hooks fire-and-forget, so provide a
        // detached one-shot runtime rather than silently dropping the effect.
        std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                runtime.block_on(run_hooks());
            }
        });
    }
}

/// Compatibility helper for shell-tool model text.
pub fn shell_model_content(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
        (true, true) => "Command completed successfully.".to_string(),
    }
}

/// Timeout selection shared by current Bash/PowerShell callers.
pub fn shell_timeout_ms(input: &serde_json::Value) -> u64 {
    let default_timeout = crate::tools::bash_tool::prompt::get_default_timeout_ms();
    input
        .get("timeout")
        .or_else(|| input.get("timeout_ms"))
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value != 0.0)
        // CC passes the schema's number directly to Node setTimeout. Negative
        // values fire immediately; positive sub-millisecond values and values
        // above Node's 32-bit delay ceiling become a 1ms timer. The advertised
        // Bash max remains prompt guidance, not a runtime clamp to that max.
        .map(|value| {
            if value < 0.0 {
                0
            } else if !(1.0..=i32::MAX as f64).contains(&value) {
                1
            } else {
                value as u64
            }
        })
        .unwrap_or(default_timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[cfg(unix)]
    #[test]
    fn output_file_open_refuses_symlink_targets() {
        let root = std::env::temp_dir().join(format!(
            "cometix-shell-output-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim");
        let output = root.join("output");
        std::fs::write(&victim, "safe").unwrap();
        std::os::unix::fs::symlink(&victim, &output).unwrap();
        assert!(open_output_file(&output).is_err());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "safe");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_override_and_provider_command_execute_with_raw_file_output() {
        struct ShellRestore(Option<EnvVarGuard>);
        impl Drop for ShellRestore {
            fn drop(&mut self) {
                drop(self.0.take());
                reset_shell_config_for_test();
            }
        }

        let _env = TEST_ENV_LOCK.lock().unwrap();
        let _restore = ShellRestore(Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1")));
        reset_shell_config_for_test();
        let command = exec(
            "printf partial; printf err >&2",
            &crate::tool::AbortController::default(),
            ShellType::Bash,
            ExecOptions {
                timeout: Some(Duration::from_secs(2)),
                cwd: Some(crate::bootstrap::state::get_original_cwd()),
                ..Default::default()
            },
        )
        .unwrap();
        let result = command.wait_result();
        assert_eq!(result.stdout, "partialerr");
        assert_eq!(result.code, 0);
    }

    #[test]
    fn cwd_effect_compares_physical_spelling_in_unicode_nfc() {
        let mut context = crate::tool::ToolUseContext::default();
        let composed = PathBuf::from("/tmp/caf\u{00e9}");
        let decomposed = PathBuf::from("/tmp/cafe\u{0301}");
        context.cwd_override = Some(composed.clone());
        apply_cwd_after_command(&mut context, decomposed);
        assert_eq!(context.cwd_override.as_ref(), Some(&composed));
    }

    #[cfg(unix)]
    #[test]
    fn shell_stdin_eof_preserves_explicit_inputs_in_both_output_modes() {
        // User-authorized L2 regression for Shell.ts:330-332. The original
        // open spawn pipe also hangs with a reader before the final &&/;
        // command: shellQuoting.ts:70's eval redirect covers only that final
        // command. Exercise actual provider -> spawn -> ShellCommand results.
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                reset_shell_config_for_test();
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-shell-stdin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("input.txt"), "provided").unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _prefix = EnvVarGuard::unset("CLAUDE_CODE_SHELL_PREFIX");
        let _restore = Restore(root.clone());
        reset_shell_config_for_test();
        for shell in ["/bin/bash", "/bin/zsh"] {
            if !Path::new(shell).is_file() {
                continue;
            }
            let _shell = EnvVarGuard::set("CLAUDE_CODE_SHELL", shell);
            reset_shell_config_for_test();
            for write_enabled in ["1", "0"] {
                let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", write_enabled);
                for (input, expected) in [
                    ("printf before && /bin/cat && printf after", "beforeafter"),
                    ("printf before; /bin/cat; printf after", "beforeafter"),
                    (
                        "printf before; /bin/cat < input.txt; printf after",
                        "beforeprovidedafter",
                    ),
                    ("/bin/cat <<'EOF'\nprovided\nEOF", "provided\n"),
                    ("/bin/cat <<< provided", "provided\n"),
                    ("printf piped | /bin/cat", "piped"),
                    ("/bin/cat <(printf substitution)", "substitution"),
                ] {
                    let command = exec(
                        input,
                        &crate::tool::AbortController::default(),
                        ShellType::Bash,
                        ExecOptions {
                            timeout: Some(Duration::from_secs(2)),
                            cwd: Some(root.clone()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    let result = command.wait_result();
                    assert_eq!(
                        result.code, 0,
                        "{shell}, writes={write_enabled}, {input}: {result:?}"
                    );
                    assert_eq!(
                        result.stdout, expected,
                        "{shell}, writes={write_enabled}, {input}"
                    );
                    assert!(result.background_task_id.is_none());
                    assert_eq!(result.cwd_after, Some(root.canonicalize().unwrap()));
                }
            }
        }
    }
}
