//! Low-level shell command execution for hooks.
//! Maps to: CC `utils/hooks.ts:747-1340#execCommandHook`.
//!
//! User-authorized structural decomposition: CC keeps this executor in its
//! ~5k-line hook monolith; Rust keeps the same setup/execution owner here.
//!
//! Partial slice: this owner currently covers the existing command/plugin/env
//! setup, POSIX-compatible spawn, timeout, cancellation, and captured result.
//! CC's shell selection, Windows Git Bash/PowerShell paths, skill context,
//! async/prompt protocol, safe-cwd fallback, and diagnostics remain deferred.

use super::CommandExecResult;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Default timeout for hook commands (seconds).
/// Maps to: CC's per-hook timeout or 30s default.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;

/// Timeout for status line commands (much shorter).
/// Maps to: CC STATUS_LINE_TIMEOUT_MS = 5000.
pub const STATUSLINE_TIMEOUT_MS: u64 = 5_000;

/// Timeout for session-end hooks.
/// Maps to: CC getSessionEndHookTimeoutMs() default 1500ms.
pub const SESSION_END_TIMEOUT_MS: u64 = 1_500;

/// Execute a shell command with JSON stdin and capture output.
/// Maps to: CC `utils/hooks.ts:747-1340#execCommandHook`.
///
/// - `command`: shell command string
/// - `json_input`: JSON payload piped to stdin
/// - `timeout`: maximum execution time
/// - `env_vars`: additional environment variables for the subprocess
/// - `plugin_root` / `plugin_id`: configured-hook substitution and environment context
/// - `abort_controller`: Rust carrier for CC's required `AbortSignal`; `None`
///   is retained only for callers that do not yet carry hook cancellation
///
/// Returns captured stdout/stderr and exit status. Dropping the pending process
/// future on timeout or cancellation drops a `kill_on_drop(true)` child.
/// Maps to: CC `executeHookCallback` (hooks.ts:4840-4890) — invoke an
/// SDK-registered callback hook and project its HookJSONOutput. The value was
/// already validated against `hook_json_output_schema` on the print leg
/// (`createHookCallback`'s sendRequest), and callback errors were mapped to
/// `{}` there, so a projection miss degrades to the empty output.
pub async fn exec_callback_hook(
    callback: &crate::schemas::hooks::HookCallback,
    json_input: &str,
    tool_use_id: Option<String>,
) -> crate::services::hooks::parsing::HookJsonOutput {
    let input: serde_json::Value =
        serde_json::from_str(json_input).unwrap_or(serde_json::Value::Null);
    let value = (callback.callback)(input, tool_use_id).await;
    serde_json::from_value(value).unwrap_or_default()
}

pub async fn exec_command_hook(
    command: &str,
    json_input: &str,
    timeout: Duration,
    mut env_vars: Vec<(String, String)>,
    plugin_root: Option<&str>,
    plugin_id: Option<&str>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> CommandExecResult {
    if abort_controller.is_some_and(crate::tool::AbortController::is_aborted) {
        return CommandExecResult {
            stdout: String::new(),
            stderr: "Hook cancelled".to_string(),
            status: -1,
            aborted: true,
        };
    }

    let execution = async move {
        // Maps to CC `execCommandHook` plugin setup (`utils/hooks.ts:801-904`).
        // Plugin variables are substituted before user-config values so values
        // containing `${CLAUDE_PLUGIN_ROOT}` remain opaque. Plugin env entries are
        // appended after caller env and therefore win on duplicate keys, matching
        // CC's object-spread override order.
        //
        // Partial: orphaned-root preflight, Windows path conversion, and
        // `getPluginDataDir`'s mkdir side effect remain outside this read-only
        // plugin slice.
        let mut command = command.to_string();
        if let Some(plugin_root) = plugin_root {
            command = command.replace("${CLAUDE_PLUGIN_ROOT}", plugin_root);
            env_vars.push(("CLAUDE_PLUGIN_ROOT".to_string(), plugin_root.to_string()));

            if let Some(plugin_id) = plugin_id {
                let data_path =
                    crate::utils::plugins::plugin_directories::plugin_data_dir_path(plugin_id)
                        .display()
                        .to_string();
                command = command.replace("${CLAUDE_PLUGIN_DATA}", &data_path);
                env_vars.push(("CLAUDE_PLUGIN_DATA".to_string(), data_path));

                let options =
                    crate::utils::plugins::plugin_options_storage::load_plugin_options(plugin_id);
                command = match crate::utils::plugins::plugin_options_storage::substitute_user_config_variables(
                    &command,
                    &options,
                ) {
                    Ok(command) => command,
                    Err(error) => {
                        return CommandExecResult {
                            stdout: String::new(),
                            stderr: format!("Failed to run: {error}"),
                            status: 1,
                            aborted: false,
                        };
                    }
                };
                for key in options.keys() {
                    let reference = format!("${{user_config.{key}}}");
                    let rendered = crate::utils::plugins::plugin_options_storage::substitute_user_config_variables(
                        &reference,
                        &options,
                    )
                    .expect("a loaded plugin-option key resolves itself");
                    let env_key = key
                        .chars()
                        .map(|character| {
                            if character.is_ascii_alphanumeric() || character == '_' {
                                character.to_ascii_uppercase()
                            } else {
                                '_'
                            }
                        })
                        .collect::<String>();
                    env_vars.push((format!("CLAUDE_PLUGIN_OPTION_{env_key}"), rendered));
                }
            }
        }

        // Keep the platform shell carrier inside the sole source-named
        // executor; no compatibility wrapper or second execution function owns
        // any part of `execCommandHook` setup.
        #[cfg(windows)]
        let mut shell = {
            let mut shell = tokio::process::Command::new("cmd");
            shell.kill_on_drop(true).args(["/C", command.as_str()]);
            shell
        };
        // CC spawns with `shell: true`, which is `/bin/sh` off Windows
        // (`hooks.ts:975-977`).
        #[cfg(not(windows))]
        let mut shell = {
            let mut shell = tokio::process::Command::new("/bin/sh");
            shell.kill_on_drop(true).args(["-c", command.as_str()]);
            shell
        };
        // CC hooks.ts:813-816,882-885: `{ ...subprocessEnv(), CLAUDE_PROJECT_DIR:
        // getProjectRoot() }`, set here for every caller; the caller's and the
        // plugin variables in `env_vars` go over it. The original cwd stands in
        // for the project root this port does not have.
        crate::utils::subprocess_env::apply_subprocess_env(&mut shell);
        shell.env(
            "CLAUDE_PROJECT_DIR",
            crate::bootstrap::state::get_original_cwd(),
        );
        let mut child = match shell
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(env_vars)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                return CommandExecResult {
                    stdout: String::new(),
                    stderr: format!("Failed to spawn hook command: {e}"),
                    status: -1,
                    aborted: false,
                };
            }
        };

        // Preserve the pre-Slice-5 process-I/O contract: write the exact JSON
        // bytes and close stdin before draining output and waiting.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(json_input.as_bytes()).await;
            drop(stdin);
        }

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let run = async move {
            let read_stdout = async move {
                let mut bytes = Vec::new();
                if let Some(stdout) = stdout.as_mut() {
                    let _ = stdout.read_to_end(&mut bytes).await;
                }
                bytes
            };
            let read_stderr = async move {
                let mut bytes = Vec::new();
                if let Some(stderr) = stderr.as_mut() {
                    let _ = stderr.read_to_end(&mut bytes).await;
                }
                bytes
            };
            let (stdout_buf, stderr_buf, status) =
                tokio::join!(read_stdout, read_stderr, child.wait());
            (stdout_buf, stderr_buf, status, child)
        };

        let timeout_fut = async {
            futures_timer::Delay::new(timeout).await;
        };

        let pinned_run = std::pin::pin!(run);
        let pinned_timeout = std::pin::pin!(timeout_fut);

        match futures::future::select(pinned_run, pinned_timeout).await {
            futures::future::Either::Left(((stdout_buf, stderr_buf, status, _child), _)) => {
                CommandExecResult {
                    stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
                    stderr: String::from_utf8_lossy(&stderr_buf).to_string(),
                    status: status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1),
                    aborted: false,
                }
            }
            futures::future::Either::Right((_, run_fut)) => {
                // Timeout: the child is inside the still-pending run future.
                // Dropping run_fut drops the child, which sends SIGKILL on Unix.
                drop(run_fut);
                CommandExecResult {
                    stdout: String::new(),
                    stderr: "Hook command timed out".to_string(),
                    status: -1,
                    aborted: true,
                }
            }
        }
    };

    let Some(abort_controller) = abort_controller else {
        return execution.await;
    };
    let wait_for_abort = async {
        while !abort_controller.is_aborted() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let execution = std::pin::pin!(execution);
    let wait_for_abort = std::pin::pin!(wait_for_abort);
    match futures::future::select(execution, wait_for_abort).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(((), execution)) => {
            // The pending execution owns the child. Dropping it preserves the
            // existing kill-on-drop cancellation behavior.
            drop(execution);
            CommandExecResult {
                stdout: String::new(),
                stderr: "Hook cancelled".to_string(),
                status: -1,
                aborted: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[tokio::test]
    async fn echo_command_captures_stdout() {
        let result = exec_command_hook(
            "echo hello",
            "{}",
            Duration::from_secs(5),
            vec![],
            None,
            None,
            None,
        )
        .await;
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout.trim(), "hello");
        assert!(!result.aborted);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn drains_large_stdout_and_stderr_concurrently() {
        let result = exec_command_hook(
            "i=0; while [ $i -lt 20000 ]; do echo stdout; echo stderr >&2; i=$((i+1)); done",
            "{}",
            Duration::from_secs(10),
            vec![],
            None,
            None,
            None,
        )
        .await;
        assert_eq!(result.status, 0);
        assert!(result.stdout.len() > 100_000);
        assert!(result.stderr.len() > 100_000);
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        let result = exec_command_hook(
            "sleep 60",
            "{}",
            Duration::from_millis(100),
            vec![],
            None,
            None,
            None,
        )
        .await;
        assert!(result.aborted);
        assert_eq!(result.status, -1);
    }

    #[tokio::test]
    async fn pre_aborted_signal_does_not_spawn_hook_process() {
        let controller = crate::tool::AbortController::default();
        controller.abort();
        let marker = std::env::temp_dir().join(format!(
            "cometix-pre-aborted-hook-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let result = exec_command_hook(
            &format!("touch '{}'", marker.display()),
            "{}",
            Duration::from_secs(5),
            vec![],
            None,
            None,
            Some(&controller),
        )
        .await;

        assert!(result.aborted);
        assert_eq!(result.stderr, "Hook cancelled");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn abort_signal_cancels_hook_process() {
        let controller = crate::tool::AbortController::default();
        let abort = controller.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            abort.abort();
        });
        let result = exec_command_hook(
            "sleep 60",
            "{}",
            Duration::from_secs(5),
            vec![],
            None,
            None,
            Some(&controller),
        )
        .await;
        assert!(result.aborted);
        assert_eq!(result.stderr, "Hook cancelled");
    }

    #[tokio::test]
    async fn json_input_piped_to_stdin_as_exact_bytes() {
        let json_input = r#"{"key":"value"}"#;
        let result = exec_command_hook(
            "cat",
            json_input,
            Duration::from_secs(5),
            vec![],
            None,
            None,
            None,
        )
        .await;
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout.as_bytes(), json_input.as_bytes());
    }

    #[tokio::test]
    async fn env_vars_passed_to_subprocess() {
        let result = exec_command_hook(
            "echo $HOOK_TEST_VAR",
            "{}",
            Duration::from_secs(5),
            vec![("HOOK_TEST_VAR".to_string(), "works".to_string())],
            None,
            None,
            None,
        )
        .await;
        assert_eq!(result.status, 0);
        assert_eq!(result.stdout.trim(), "works");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn plugin_substitution_and_environment_match_official_exec_setup() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let plugin_root = std::env::temp_dir().join(format!(
            "cometix-hook-plugin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&plugin_root).unwrap();
        let plugin_root = plugin_root.display().to_string();
        let plugin_id = format!("demo-{}@marketplace", uuid::Uuid::new_v4().simple());
        let plugin_data =
            crate::utils::plugins::plugin_directories::plugin_data_dir_path(&plugin_id)
                .display()
                .to_string();
        let result = exec_command_hook(
            r#"printf '%s\n%s\n%s\n%s\n' '${CLAUDE_PLUGIN_ROOT}' "$CLAUDE_PLUGIN_ROOT" '${CLAUDE_PLUGIN_DATA}' "$CLAUDE_PLUGIN_DATA""#,
            "{}",
            Duration::from_secs(5),
            vec![
                ("CLAUDE_PLUGIN_ROOT".to_string(), "stale-root".to_string()),
                ("CLAUDE_PLUGIN_DATA".to_string(), "stale-data".to_string()),
            ],
            Some(&plugin_root),
            Some(&plugin_id),
            None,
        )
        .await;

        assert_eq!(result.status, 0, "{}", result.stderr);
        assert_eq!(
            result.stdout.lines().collect::<Vec<_>>(),
            vec![
                plugin_root.as_str(),
                plugin_root.as_str(),
                plugin_data.as_str(),
                plugin_data.as_str(),
            ]
        );
        let _ = std::fs::remove_dir_all(plugin_root);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn plugin_options_are_substituted_after_plugin_paths_and_override_base_env() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let config_root = std::env::temp_dir().join(format!(
            "cometix-hook-config-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&config_root).unwrap();
        let plugin_id = format!("options-{}@marketplace", uuid::Uuid::new_v4().simple());
        let settings = serde_json::json!({
            "pluginConfigs": {
                (plugin_id.clone()): {
                    "options": {
                        "opaque": "${CLAUDE_PLUGIN_ROOT}",
                        "count": 42
                    }
                }
            }
        });
        std::fs::write(
            config_root.join("settings.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_root);

        let plugin_root = std::env::temp_dir().display().to_string();
        let result = exec_command_hook(
            r#"printf '%s\n%s\n%s\n' '${user_config.opaque}' "$CLAUDE_PLUGIN_OPTION_OPAQUE" "$CLAUDE_PLUGIN_OPTION_COUNT""#,
            "{}",
            Duration::from_secs(5),
            vec![(
                "CLAUDE_PLUGIN_OPTION_OPAQUE".to_string(),
                "stale".to_string(),
            )],
            Some(&plugin_root),
            Some(&plugin_id),
            None,
        )
        .await;

        let _ = std::fs::remove_dir_all(config_root);
        assert_eq!(result.status, 0, "{}", result.stderr);
        assert_eq!(
            result.stdout.lines().collect::<Vec<_>>(),
            vec!["${CLAUDE_PLUGIN_ROOT}", "${CLAUDE_PLUGIN_ROOT}", "42"]
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn abort_argument_uses_the_same_plugin_setup_path() {
        let plugin_root = std::env::temp_dir().display().to_string();
        let controller = crate::tool::AbortController::default();
        let result = exec_command_hook(
            r#"printf '%s\n%s\n' '${CLAUDE_PLUGIN_ROOT}' "$CLAUDE_PLUGIN_ROOT""#,
            "{}",
            Duration::from_secs(5),
            vec![],
            Some(&plugin_root),
            None,
            Some(&controller),
        )
        .await;

        assert_eq!(result.status, 0, "{}", result.stderr);
        assert_eq!(
            result.stdout.lines().collect::<Vec<_>>(),
            vec![plugin_root.as_str(), plugin_root.as_str()]
        );
    }
}
