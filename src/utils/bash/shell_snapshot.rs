//! User shell snapshot creation.
//!
//! Maps to: CC `utils/bash/ShellSnapshot.ts:1-430`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SNAPSHOT_CREATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Maps to CC `utils/bash/ShellSnapshot.ts:181-192` `getConfigFile`.
fn get_config_file(shell_path: &str) -> PathBuf {
    let file = if shell_path.contains("zsh") {
        ".zshrc"
    } else if shell_path.contains("bash") {
        ".bashrc"
    } else {
        ".profile"
    };
    crate::utils::node_os::homedir().join(file)
}

fn create_argv0_shell_function(
    function_name: &str,
    argv0: &str,
    binary_path: &str,
    prepend_args: &[&str],
) -> String {
    let path = crate::utils::bash::shell_quote::quote(&[binary_path]);
    let suffix = if prepend_args.is_empty() {
        "\"$@\"".to_string()
    } else {
        format!("{} \"$@\"", prepend_args.join(" "))
    };
    [
        format!("function {function_name} {{"),
        "  if [[ -n $ZSH_VERSION ]]; then".to_string(),
        format!("    ARGV0={argv0} {path} {suffix}"),
        "  elif [[ \"$OSTYPE\" == \"msys\" ]] || [[ \"$OSTYPE\" == \"cygwin\" ]] || [[ \"$OSTYPE\" == \"win32\" ]]; then".to_string(),
        format!("    ARGV0={argv0} {path} {suffix}"),
        "  elif [[ $BASHPID != $$ ]]; then".to_string(),
        format!("    exec -a {argv0} {path} {suffix}"),
        "  else".to_string(),
        format!("    (exec -a {argv0} {path} {suffix})"),
        "  fi".to_string(),
        "}".to_string(),
    ]
    .join("\n")
}

/// Maps to CC `createRipgrepShellIntegration()`.
pub fn create_ripgrep_shell_integration() -> (String, String) {
    let command = crate::utils::ripgrep::ripgrep_command();
    if let Some(argv0) = command.argv0.as_deref() {
        return (
            "function".to_string(),
            create_argv0_shell_function("rg", argv0, &command.rg_path, &[]),
        );
    }
    let path = crate::utils::bash::shell_quote::quote(&[&command.rg_path]);
    let args = command
        .rg_args
        .iter()
        .map(|arg| crate::utils::bash::shell_quote::quote(&[arg]))
        .collect::<Vec<_>>();
    let snippet = if args.is_empty() {
        path
    } else {
        format!("{path} {}", args.join(" "))
    };
    ("alias".to_string(), snippet)
}

/// Maps to CC `createFindGrepShellIntegration()`.
pub fn create_find_grep_shell_integration() -> Option<String> {
    if !crate::utils::embedded_tools::has_embedded_search_tools() {
        return None;
    }
    let path = crate::utils::embedded_tools::embedded_search_tools_binary_path()
        .display()
        .to_string();
    let vcs = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
    let mut grep_args = vec!["-G", "--ignore-files", "--hidden", "-I"];
    let exclusions = vcs
        .iter()
        .map(|directory| format!("--exclude-dir={directory}"))
        .collect::<Vec<_>>();
    let exclusion_refs = exclusions.iter().map(String::as_str).collect::<Vec<_>>();
    grep_args.extend(exclusion_refs);
    Some(
        [
            "unalias find 2>/dev/null || true".to_string(),
            "unalias grep 2>/dev/null || true".to_string(),
            create_argv0_shell_function("find", "bfs", &path, &["-regextype", "findutils-default"]),
            create_argv0_shell_function("grep", "ugrep", &path, &grep_args),
        ]
        .join("\n"),
    )
}

fn get_user_snapshot_content(config_file: &Path) -> String {
    let is_zsh = config_file.ends_with(".zshrc");
    let functions = if is_zsh {
        r##"
      echo "# Functions" >> "$SNAPSHOT_FILE"
      typeset -f > /dev/null 2>&1
      typeset +f | grep -vE '^_[^_]' | while read func; do
        typeset -f "$func" >> "$SNAPSHOT_FILE"
      done
"##
    } else {
        r##"
      echo "# Functions" >> "$SNAPSHOT_FILE"
      declare -f > /dev/null 2>&1
      declare -F | cut -d' ' -f3 | grep -vE '^_[^_]' | while read func; do
        encoded_func=$(declare -f "$func" | base64 )
        echo "eval \"\$(echo '$encoded_func' | base64 -d)\" > /dev/null 2>&1" >> "$SNAPSHOT_FILE"
      done
"##
    };
    let options = if is_zsh {
        r##"
      echo "# Shell Options" >> "$SNAPSHOT_FILE"
      setopt | sed 's/^/setopt /' | head -n 1000 >> "$SNAPSHOT_FILE"
"##
    } else {
        r##"
      echo "# Shell Options" >> "$SNAPSHOT_FILE"
      shopt -p | head -n 1000 >> "$SNAPSHOT_FILE"
      set -o | grep "on" | awk '{print "set -o " $1}' | head -n 1000 >> "$SNAPSHOT_FILE"
      echo "shopt -s expand_aliases" >> "$SNAPSHOT_FILE"
"##
    };
    format!(
        r##"{functions}{options}
      echo "# Aliases" >> "$SNAPSHOT_FILE"
      if [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
        alias | grep -v "='winpty " | sed 's/^alias //g' | sed 's/^/alias -- /' | head -n 1000 >> "$SNAPSHOT_FILE"
      else
        alias | sed 's/^alias //g' | sed 's/^/alias -- /' | head -n 1000 >> "$SNAPSHOT_FILE"
      fi
"##
    )
}

/// Maps to CC `utils/bash/ShellSnapshot.ts:269-340` `getClaudeCodeSnapshotContent`.
fn get_claude_code_snapshot_content(env: &crate::utils::process_env::EnvSnapshot) -> String {
    let (integration_type, rg_integration) = create_ripgrep_shell_integration();
    let mut content = String::from(
        r##"
      echo "# Check for rg availability" >> "$SNAPSHOT_FILE"
      echo "if ! (unalias rg 2>/dev/null; command -v rg) >/dev/null 2>&1; then" >> "$SNAPSHOT_FILE"
"##,
    );
    if integration_type == "function" {
        content.push_str(&format!(
            "      cat >> \"$SNAPSHOT_FILE\" << 'RIPGREP_FUNC_END'\n{rg_integration}\nRIPGREP_FUNC_END\n"
        ));
    } else {
        let escaped = rg_integration.replace('\'', "'\\''");
        content.push_str(&format!(
            "      echo '  alias rg='\"'{escaped}'\" >> \"$SNAPSHOT_FILE\"\n"
        ));
    }
    content.push_str("      echo \"fi\" >> \"$SNAPSHOT_FILE\"\n");
    if let Some(find_grep) = create_find_grep_shell_integration() {
        content.push_str(&format!(
            "      echo \"# Shadow find/grep with embedded bfs/ugrep\" >> \"$SNAPSHOT_FILE\"\n      cat >> \"$SNAPSHOT_FILE\" << 'FIND_GREP_FUNC_END'\n{find_grep}\nFIND_GREP_FUNC_END\n"
        ));
    }
    let path = env.var("PATH").unwrap_or_default();
    content.push_str(&format!(
        "      echo \"export PATH={}\" >> \"$SNAPSHOT_FILE\"\n",
        crate::utils::bash::shell_quote::quote(&[&path])
    ));
    content
}

/// Maps to CC `utils/bash/ShellSnapshot.ts:345-395` `getSnapshotScript`.
fn get_snapshot_script(
    shell_path: &str,
    snapshot_path: &Path,
    config_exists: bool,
    env: &crate::utils::process_env::EnvSnapshot,
) -> String {
    let config = get_config_file(shell_path);
    let is_zsh = config.ends_with(".zshrc");
    let user = if config_exists {
        get_user_snapshot_content(&config)
    } else if !is_zsh {
        "echo \"shopt -s expand_aliases\" >> \"$SNAPSHOT_FILE\"".to_string()
    } else {
        String::new()
    };
    let source = if config_exists {
        format!(
            "source {} < /dev/null",
            crate::utils::bash::shell_quote::quote(&[&config.display().to_string()])
        )
    } else {
        "# No user config file to source".to_string()
    };
    format!(
        r##"SNAPSHOT_FILE={snapshot}
      {source}
      echo "# Snapshot file" >| "$SNAPSHOT_FILE"
      echo "# Unset all aliases to avoid conflicts with functions" >> "$SNAPSHOT_FILE"
      echo "unalias -a 2>/dev/null || true" >> "$SNAPSHOT_FILE"
      {user}
      {claude}
      if [ ! -f "$SNAPSHOT_FILE" ]; then
        echo "Error: Snapshot file was not created at $SNAPSHOT_FILE" >&2
        exit 1
      fi
"##,
        snapshot = crate::utils::bash::shell_quote::quote(&[&snapshot_path.display().to_string()]),
        claude = get_claude_code_snapshot_content(env),
    )
}

fn run_snapshot_command(
    command: &mut std::process::Command,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                let mut taskkill = std::process::Command::new("taskkill");
                // Rust-only tree kill; every child takes the carrier (process.env) as its base.
                crate::utils::subprocess_env::apply_process_env_std(&mut taskkill);
                let _ = taskkill
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .status();
            }
            let _ = child.kill();
            return child.wait();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Maps to CC `createAndSaveSnapshot(binShell)`.
pub fn create_and_save_snapshot(bin_shell: &str) -> Option<PathBuf> {
    let process_env = crate::utils::process_env::snapshot();
    if !crate::utils::session_storage::is_session_write_enabled() {
        return None;
    }
    let shell_type = if bin_shell.contains("zsh") {
        "zsh"
    } else if bin_shell.contains("bash") {
        "bash"
    } else {
        "sh"
    };
    let config = get_config_file(bin_shell);
    let snapshots_dir =
        crate::utils::env_utils::get_claude_config_home_dir().join("shell-snapshots");
    if std::fs::create_dir_all(&snapshots_dir).is_err() {
        return None;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let snapshot = snapshots_dir.join(format!(
        "snapshot-{shell_type}-{timestamp}-{}.sh",
        uuid::Uuid::new_v4().simple()
    ));
    let script = get_snapshot_script(bin_shell, &snapshot, config.is_file(), &process_env);

    let mut command = std::process::Command::new(bin_shell);
    // CC `ShellSnapshot.ts:460-467`: the base env is `subprocessEnv()` unless
    // CLAUDE_CODE_DONT_INHERIT_ENV is set (then an empty env); SHELL /
    // GIT_EDITOR / CLAUDECODE are spread over that base afterwards.
    if process_env
        .var_os("CLAUDE_CODE_DONT_INHERIT_ENV")
        .is_some_and(|value| !value.is_empty())
    {
        command.env_clear();
    } else {
        crate::utils::subprocess_env::apply_subprocess_env_std_snapshot(&mut command, &process_env);
    }
    command
        .args(["-c", "-l", &script])
        .env("SHELL", bin_shell)
        .env("GIT_EDITOR", "true")
        .env("CLAUDECODE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let run = run_snapshot_command(&mut command, SNAPSHOT_CREATION_TIMEOUT);
    match run {
        Ok(status) if status.success() && snapshot.is_file() => Some(snapshot),
        _ => {
            let _ = std::fs::remove_file(&snapshot);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// CC `utils/bash/ShellSnapshot.ts:269-340` captures the current
    /// `process.env.PATH` in the generated snapshot script.
    #[test]
    fn shell_integrations_match_official_snapshot_shape_and_path_capture() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _path = EnvVarGuard::set("PATH", "/cometix/carrier-only-snapshot-path");
        let env = crate::utils::process_env::snapshot();
        let (kind, snippet) = create_ripgrep_shell_integration();
        assert!(matches!(kind.as_str(), "alias" | "function"));
        assert!(!snippet.is_empty());
        let script = get_snapshot_script("/bin/bash", Path::new("/tmp/snapshot.sh"), false, &env);
        assert!(script.contains("shopt -s expand_aliases"));
        assert!(script.contains("unalias -a"));
        assert!(script.contains("/cometix/carrier-only-snapshot-path"));
    }
}
