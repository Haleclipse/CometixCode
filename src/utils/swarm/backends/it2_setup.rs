//! iTerm2 `it2` setup helpers.
//! Maps to: CC `utils/swarm/backends/it2Setup.ts`.
//!
//! The functions in this module keep the same non-UI boundary as Claude Code:
//! package-manager detection, optional `it2` installation, verification of the
//! iTerm2 Python API, user-facing setup instructions, and global-config flags
//! that suppress repeated setup prompts or prefer tmux. Callers must still own
//! the interactive prompt flow (`It2SetupPrompt.tsx`).

use std::path::PathBuf;
use std::process::Command;

/// Maps to: CC `PythonPackageManager`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PythonPackageManager {
    Uvx,
    Pipx,
    Pip,
}

impl PythonPackageManager {
    /// Maps to the official string literal values in `it2Setup.ts`.
    pub fn official_name(self) -> &'static str {
        match self {
            Self::Uvx => "uvx",
            Self::Pipx => "pipx",
            Self::Pip => "pip",
        }
    }
}

/// Maps to: CC `It2InstallResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct It2InstallResult {
    pub success: bool,
    pub error: Option<String>,
    pub package_manager: Option<PythonPackageManager>,
}

/// Maps to: CC `It2VerifyResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct It2VerifyResult {
    pub success: bool,
    pub error: Option<String>,
    pub needs_python_api_enabled: Option<bool>,
}

/// Maps to the `{ stdout, stderr, code }` results returned by CC
/// `execFileNoThrow(...)` calls in `it2Setup.ts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

fn run_command(program: &str, args: &[&str], cwd: Option<PathBuf>) -> SetupCommandResult {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    match command.output() {
        Ok(output) => SetupCommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(1),
        },
        Err(error) => SetupCommandResult {
            stdout: String::new(),
            stderr: error.to_string(),
            code: 1,
        },
    }
}

fn home_dir() -> PathBuf {
    crate::utils::process_env::var_os("HOME")
        .or_else(|| crate::utils::process_env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pure decision table for CC `detectPythonPackageManager()`.
pub fn detect_python_package_manager_from_results(
    uv_code: i32,
    pipx_code: i32,
    pip_code: i32,
    pip3_code: i32,
) -> Option<PythonPackageManager> {
    if uv_code == 0 {
        Some(PythonPackageManager::Uvx)
    } else if pipx_code == 0 {
        Some(PythonPackageManager::Pipx)
    } else if pip_code == 0 || pip3_code == 0 {
        Some(PythonPackageManager::Pip)
    } else {
        None
    }
}

/// Maps to: CC `detectPythonPackageManager()`.
pub async fn detect_python_package_manager() -> Option<PythonPackageManager> {
    let uv_result = run_command("which", &["uv"], None);
    if uv_result.code == 0 {
        return Some(PythonPackageManager::Uvx);
    }

    let pipx_result = run_command("which", &["pipx"], None);
    if pipx_result.code == 0 {
        return Some(PythonPackageManager::Pipx);
    }

    let pip_result = run_command("which", &["pip"], None);
    if pip_result.code == 0 {
        return Some(PythonPackageManager::Pip);
    }

    let pip3_result = run_command("which", &["pip3"], None);
    if pip3_result.code == 0 {
        return Some(PythonPackageManager::Pip);
    }

    None
}

/// Maps to: CC `isIt2CliAvailable()` in `it2Setup.ts` (PATH-only check).
pub async fn is_it2_cli_installed() -> bool {
    run_command("which", &["it2"], None).code == 0
}

/// Maps to: CC `installIt2(...)` command selection.
pub fn install_command_for_package_manager(
    package_manager: PythonPackageManager,
) -> (&'static str, Vec<&'static str>) {
    match package_manager {
        PythonPackageManager::Uvx => ("uv", vec!["tool", "install", "it2"]),
        PythonPackageManager::Pipx => ("pipx", vec!["install", "it2"]),
        PythonPackageManager::Pip => ("pip", vec!["install", "--user", "it2"]),
    }
}

/// Maps to: CC `installIt2(...)`.
///
/// This performs the same explicit package-manager command that the official
/// setup prompt invokes, running from the user's home directory to avoid
/// project-local package-manager config. It is not called automatically by
/// backend detection.
pub async fn install_it2(package_manager: PythonPackageManager) -> It2InstallResult {
    let (program, args) = install_command_for_package_manager(package_manager);
    let mut result = run_command(program, &args, Some(home_dir()));
    if package_manager == PythonPackageManager::Pip && result.code != 0 {
        result = run_command("pip3", &["install", "--user", "it2"], Some(home_dir()));
    }

    if result.code != 0 {
        It2InstallResult {
            success: false,
            error: Some(if result.stderr.is_empty() {
                "Unknown installation error".to_string()
            } else {
                result.stderr
            }),
            package_manager: Some(package_manager),
        }
    } else {
        It2InstallResult {
            success: true,
            error: None,
            package_manager: Some(package_manager),
        }
    }
}

/// Maps to: CC `verifyIt2Setup()` failure classification.
pub fn verify_it2_result_from_command(
    installed: bool,
    session_list_result: SetupCommandResult,
) -> It2VerifyResult {
    if !installed {
        return It2VerifyResult {
            success: false,
            error: Some("it2 CLI is not installed or not in PATH".to_string()),
            needs_python_api_enabled: None,
        };
    }

    if session_list_result.code == 0 {
        return It2VerifyResult {
            success: true,
            error: None,
            needs_python_api_enabled: None,
        };
    }

    let stderr = session_list_result.stderr.to_ascii_lowercase();
    if stderr.contains("api")
        || stderr.contains("python")
        || stderr.contains("connection refused")
        || stderr.contains("not enabled")
    {
        return It2VerifyResult {
            success: false,
            error: Some("Python API not enabled in iTerm2 preferences".to_string()),
            needs_python_api_enabled: Some(true),
        };
    }

    It2VerifyResult {
        success: false,
        error: Some(if session_list_result.stderr.is_empty() {
            "Failed to communicate with iTerm2".to_string()
        } else {
            session_list_result.stderr
        }),
        needs_python_api_enabled: None,
    }
}

/// Maps to: CC `verifyIt2Setup()`.
pub async fn verify_it2_setup() -> It2VerifyResult {
    let installed = is_it2_cli_installed().await;
    let result = if installed {
        run_command("it2", &["session", "list"], None)
    } else {
        SetupCommandResult {
            stdout: String::new(),
            stderr: String::new(),
            code: 1,
        }
    };
    verify_it2_result_from_command(installed, result)
}

/// Maps to: CC `getPythonApiInstructions()`.
pub fn get_python_api_instructions() -> Vec<&'static str> {
    vec![
        "Almost done! Enable the Python API in iTerm2:",
        "",
        "  iTerm2 → Settings → General → Magic → Enable Python API",
        "",
        "After enabling, you may need to restart iTerm2.",
    ]
}

/// Maps to: CC `markIt2SetupComplete()`.
pub fn mark_it2_setup_complete() -> anyhow::Result<()> {
    if crate::utils::config::load_global_config().iterm2_it2_setup_complete != Some(true) {
        crate::utils::config::save_global_config(|config| {
            config.iterm2_it2_setup_complete = Some(true);
        })?;
    }
    Ok(())
}

/// Maps to: CC `setPreferTmuxOverIterm2(...)`.
pub fn set_prefer_tmux_over_iterm2(prefer: bool) -> anyhow::Result<()> {
    if crate::utils::config::load_global_config().prefer_tmux_over_iterm2 != Some(prefer) {
        crate::utils::config::save_global_config(|config| {
            config.prefer_tmux_over_iterm2 = Some(prefer);
        })?;
    }
    Ok(())
}

/// Maps to: CC `getPreferTmuxOverIterm2()`.
pub fn get_prefer_tmux_over_iterm2() -> bool {
    crate::utils::config::load_global_config().prefer_tmux_over_iterm2 == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, value),
            }
        }
    }

    fn temp_config_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cometix-it2-setup-{name}-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn package_manager_detection_matches_official_preference_order() {
        assert_eq!(
            detect_python_package_manager_from_results(0, 0, 0, 0),
            Some(PythonPackageManager::Uvx)
        );
        assert_eq!(
            detect_python_package_manager_from_results(1, 0, 0, 0),
            Some(PythonPackageManager::Pipx)
        );
        assert_eq!(
            detect_python_package_manager_from_results(1, 1, 0, 1),
            Some(PythonPackageManager::Pip)
        );
        assert_eq!(
            detect_python_package_manager_from_results(1, 1, 1, 0),
            Some(PythonPackageManager::Pip)
        );
        assert_eq!(detect_python_package_manager_from_results(1, 1, 1, 1), None);
    }

    #[test]
    fn install_command_selection_matches_official_it2_setup() {
        assert_eq!(
            install_command_for_package_manager(PythonPackageManager::Uvx),
            ("uv", vec!["tool", "install", "it2"])
        );
        assert_eq!(
            install_command_for_package_manager(PythonPackageManager::Pipx),
            ("pipx", vec!["install", "it2"])
        );
        assert_eq!(
            install_command_for_package_manager(PythonPackageManager::Pip),
            ("pip", vec!["install", "--user", "it2"])
        );
    }

    #[test]
    fn verify_it2_setup_classifies_missing_python_api_like_official() {
        let missing = verify_it2_result_from_command(
            false,
            SetupCommandResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 1,
            },
        );
        assert!(!missing.success);
        assert_eq!(
            missing.error.as_deref(),
            Some("it2 CLI is not installed or not in PATH")
        );

        let api_disabled = verify_it2_result_from_command(
            true,
            SetupCommandResult {
                stdout: String::new(),
                stderr: "Connection refused: Python API not enabled".to_string(),
                code: 1,
            },
        );
        assert!(!api_disabled.success);
        assert_eq!(api_disabled.needs_python_api_enabled, Some(true));
        assert_eq!(
            api_disabled.error.as_deref(),
            Some("Python API not enabled in iTerm2 preferences")
        );

        let ok = verify_it2_result_from_command(
            true,
            SetupCommandResult {
                stdout: "SESSION".to_string(),
                stderr: String::new(),
                code: 0,
            },
        );
        assert!(ok.success);
    }

    #[test]
    fn python_api_instructions_match_official_copy() {
        let instructions = get_python_api_instructions();
        assert!(instructions[0].contains("Enable the Python API"));
        assert!(instructions[2].contains("iTerm2 → Settings → General → Magic"));
        assert!(instructions[4].contains("restart iTerm2"));
    }

    #[test]
    fn it2_setup_config_flags_match_official_global_config_keys() {
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let dir = temp_config_dir("flags");
        let _config_guard = EnvGuard::set("CLAUDE_CONFIG_DIR", &dir);
        let _session_write_guard = EnvGuard::set("SESSION_WRITE_ENABLED", "1");
        let _write_guard = EnvGuard::set("COMETIX_WRITE_ENABLED", "1");

        assert!(!get_prefer_tmux_over_iterm2());
        mark_it2_setup_complete().unwrap();
        set_prefer_tmux_over_iterm2(true).unwrap();

        let config = crate::utils::config::load_global_config();
        assert_eq!(config.iterm2_it2_setup_complete, Some(true));
        assert_eq!(config.prefer_tmux_over_iterm2, Some(true));

        let raw = std::fs::read_to_string(crate::utils::config::get_global_config_path()).unwrap();
        assert!(raw.contains("iterm2It2SetupComplete"));
        assert!(raw.contains("preferTmuxOverIterm2"));
    }
}
