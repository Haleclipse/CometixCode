//! Bash/zsh shell provider.
//!
//! Maps to: CC `utils/shell/bashProvider.ts:1-247`.

use std::path::PathBuf;
use std::sync::Mutex;

use super::shell_provider::{BuiltExecCommand, ShellProvider, ShellType};
use crate::utils::process_env::JsTruthy;

pub struct BashShellProvider {
    shell_path: PathBuf,
    snapshot_file_path: Mutex<Option<PathBuf>>,
    current_sandbox_tmp_dir: Mutex<Option<PathBuf>>,
}

impl BashShellProvider {
    /// Maps to the cleanup registered by `createAndSaveSnapshot` after a
    /// successful snapshot creation.
    pub fn cleanup_snapshot(&self) {
        if let Ok(mut snapshot) = self.snapshot_file_path.lock() {
            if let Some(path) = snapshot.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl std::fmt::Debug for BashShellProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BashShellProvider")
            .field("shell_path", &self.shell_path)
            .finish_non_exhaustive()
    }
}

fn get_disable_extglob_command(shell_path: &str) -> Option<&'static str> {
    if crate::utils::process_env::var_os("CLAUDE_CODE_SHELL_PREFIX")
        .as_deref()
        .truthy()
        .is_some()
    {
        return Some("{ shopt -u extglob || setopt NO_EXTENDED_GLOB; } >/dev/null 2>&1 || true");
    }
    if shell_path.contains("bash") {
        Some("shopt -u extglob 2>/dev/null || true")
    } else if shell_path.contains("zsh") {
        Some("setopt NO_EXTENDED_GLOB 2>/dev/null || true")
    } else {
        None
    }
}

/// Maps to CC `createBashShellProvider(shellPath, options)`.
pub fn create_bash_shell_provider(shell_path: PathBuf, skip_snapshot: bool) -> BashShellProvider {
    let snapshot = (!skip_snapshot).then(|| {
        crate::utils::bash::shell_snapshot::create_and_save_snapshot(
            &shell_path.display().to_string(),
        )
    });
    BashShellProvider {
        shell_path,
        snapshot_file_path: Mutex::new(snapshot.flatten()),
        current_sandbox_tmp_dir: Mutex::new(None),
    }
}

impl ShellProvider for BashShellProvider {
    fn shell_type(&self) -> ShellType {
        ShellType::Bash
    }

    fn shell_path(&self) -> &std::path::Path {
        &self.shell_path
    }

    fn detached(&self) -> bool {
        true
    }

    /// Maps to CC `bashProvider.buildExecCommand(command, opts)`.
    fn build_exec_command(
        &self,
        command: &str,
        id: &str,
        sandbox_tmp_dir: Option<&std::path::Path>,
        use_sandbox: bool,
    ) -> std::io::Result<BuiltExecCommand> {
        let mut snapshot = self
            .snapshot_file_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if snapshot.as_ref().is_some_and(|path| !path.is_file()) {
            *snapshot = None;
        }
        if let Ok(mut current) = self.current_sandbox_tmp_dir.lock() {
            *current = sandbox_tmp_dir.map(std::path::Path::to_path_buf);
        }

        // CC `bashProvider.ts:108` `osTmpdir()`.
        let native_tmp = crate::utils::node_os::tmpdir();
        let shell_tmp = if cfg!(windows) {
            PathBuf::from(crate::utils::windows_paths::windows_path_to_posix_path(
                &native_tmp.display().to_string(),
            ))
        } else {
            native_tmp.clone()
        };
        let shell_cwd_file_path = if use_sandbox {
            sandbox_tmp_dir
                .expect("sandbox tmp dir required when sandbox is enabled")
                .join(format!("cwd-{id}"))
        } else {
            shell_tmp.join(format!("claude-{id}-cwd"))
        };
        let cwd_file_path = if use_sandbox {
            shell_cwd_file_path.clone()
        } else {
            native_tmp.join(format!("claude-{id}-cwd"))
        };

        let normalized = crate::utils::bash::shell_quoting::rewrite_windows_null_redirect(command);
        let add_stdin_redirect =
            crate::utils::bash::shell_quoting::should_add_stdin_redirect(&normalized);
        let mut quoted =
            crate::utils::bash::shell_quoting::quote_shell_command(&normalized, add_stdin_redirect);
        if normalized.contains('|') && add_stdin_redirect {
            quoted = crate::utils::bash::bash_pipe_command::rearrange_pipe_command(&normalized);
        }

        let mut parts = Vec::new();
        if let Some(path) = snapshot.as_ref() {
            let shell_path = if cfg!(windows) {
                crate::utils::windows_paths::windows_path_to_posix_path(&path.display().to_string())
            } else {
                path.display().to_string()
            };
            parts.push(format!(
                "source {} 2>/dev/null || true",
                crate::utils::bash::shell_quote::quote(&[&shell_path])
            ));
        }
        if let Some(script) = crate::utils::session_environment::get_session_environment_script() {
            parts.push(script);
        }
        if let Some(disable_extglob) =
            get_disable_extglob_command(&self.shell_path.display().to_string())
        {
            parts.push(disable_extglob.to_string());
        }
        parts.push(format!("eval {quoted}"));
        let shell_cwd = if cfg!(windows) {
            crate::utils::windows_paths::windows_path_to_posix_path(
                &shell_cwd_file_path.display().to_string(),
            )
        } else {
            shell_cwd_file_path.display().to_string()
        };
        parts.push(format!(
            "pwd -P >| {}",
            crate::utils::bash::shell_quote::quote(&[&shell_cwd])
        ));
        let mut command_string = parts.join(" && ");
        if let Some(prefix) = crate::utils::process_env::var("CLAUDE_CODE_SHELL_PREFIX").truthy() {
            command_string = crate::utils::bash::shell_prefix::format_shell_prefix_command(
                &prefix,
                &command_string,
            );
        }

        Ok(BuiltExecCommand {
            command_string,
            cwd_file_path,
        })
    }

    /// Maps to CC `bashProvider.getSpawnArgs(commandString)`.
    fn get_spawn_args(&self, command_string: &str) -> Vec<String> {
        let snapshot_exists = self
            .snapshot_file_path
            .lock()
            .map(|snapshot| snapshot.is_some())
            .unwrap_or(false);
        let mut args = vec!["-c".to_string()];
        if !snapshot_exists {
            args.push("-l".to_string());
        }
        args.push(command_string.to_string());
        args
    }

    /// Maps to CC `bashProvider.getEnvironmentOverrides(command)`.
    fn get_environment_overrides(&self, _command: &str) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Ok(current) = self.current_sandbox_tmp_dir.lock() {
            if let Some(path) = current.as_ref() {
                let path = if cfg!(windows) {
                    crate::utils::windows_paths::windows_path_to_posix_path(
                        &path.display().to_string(),
                    )
                } else {
                    path.display().to_string()
                };
                env.push(("TMPDIR".to_string(), path.clone()));
                env.push(("CLAUDE_CODE_TMPDIR".to_string(), path.clone()));
                env.push(("TMPPREFIX".to_string(), format!("{path}/zsh")));
            }
        }
        env.extend(crate::utils::session_env_vars::get_session_env_vars());
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    #[test]
    fn provider_builds_official_eval_snapshot_and_cwd_trailer_shape() {
        let provider = create_bash_shell_provider(PathBuf::from("/bin/bash"), true);
        let built = provider
            .build_exec_command("printf hi | wc -c", "abcd", None, false)
            .unwrap();
        assert!(built.command_string.contains("shopt -u extglob"));
        assert!(
            built
                .command_string
                .contains("eval 'printf hi < /dev/null | wc -c'")
        );
        assert!(built.command_string.contains("pwd -P >|"));
        assert_eq!(
            provider.get_spawn_args(&built.command_string)[..2],
            ["-c", "-l"]
        );
    }

    #[test]
    fn successful_snapshot_is_removed_by_provider_cleanup() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-shell-snapshot-cleanup-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let provider = create_bash_shell_provider(PathBuf::from("/bin/bash"), false);
        let snapshot = provider
            .snapshot_file_path
            .lock()
            .unwrap()
            .clone()
            .expect("snapshot created");
        assert!(snapshot.is_file());
        provider.cleanup_snapshot();
        assert!(!snapshot.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
