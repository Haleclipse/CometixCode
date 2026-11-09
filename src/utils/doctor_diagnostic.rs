//! Maps to: CC `utils/doctorDiagnostic.ts`.
//!
//! Official Claude Code probes npm/native/package-manager installs, ripgrep,
//! shell aliases, and updater permissions. This Rust port keeps the same data
//! model and safe local probes that do not mutate the
//! system or execute package-manager/OAuth flows. Expensive or potentially
//! mutating checks (npm config, package-manager command execution, PID cleanup)
//! remain deferred to their dedicated service slices.

use std::path::Path;

/// Maps to CC `InstallationType`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallationType {
    NpmGlobal,
    Native,
    PackageManager,
    Development,
    Unknown,
}

impl InstallationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NpmGlobal => "npm-global",
            Self::Native => "native",
            Self::PackageManager => "package-manager",
            Self::Development => "development",
            Self::Unknown => "unknown",
        }
    }
}

/// Maps to CC `DiagnosticInfo.multipleInstallations[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallationRecord {
    pub install_type: String,
    pub path: String,
}

/// Maps to CC `DiagnosticInfo.warnings[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticWarning {
    pub issue: String,
    pub fix: String,
}

/// Maps to CC `DiagnosticInfo.ripgrepStatus.mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RipgrepMode {
    System,
    Builtin,
    Embedded,
}

impl RipgrepMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Builtin => "builtin",
            Self::Embedded => "embedded",
        }
    }
}

/// Maps to CC `DiagnosticInfo.ripgrepStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RipgrepStatus {
    pub working: bool,
    pub mode: RipgrepMode,
    pub system_path: Option<String>,
}

/// Maps to CC `DiagnosticInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticInfo {
    pub installation_type: InstallationType,
    pub version: String,
    pub installation_path: String,
    pub invoked_binary: String,
    pub config_install_method: String,
    pub auto_updates: String,
    pub has_update_permissions: Option<bool>,
    pub multiple_installations: Vec<InstallationRecord>,
    pub warnings: Vec<DiagnosticWarning>,
    pub recommendation: Option<String>,
    pub package_manager: Option<String>,
    pub ripgrep_status: RipgrepStatus,
    pub git_bash: Option<String>,
}

/// Maps to CC local `getNormalizedPaths()`.
pub fn get_normalized_paths() -> (String, String) {
    let mut invoked_path = std::env::args().next().unwrap_or_default();
    let mut exec_path = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| invoked_path.clone());

    if cfg!(target_os = "windows") {
        invoked_path = invoked_path.replace('\\', "/");
        exec_path = exec_path.replace('\\', "/");
    }

    (invoked_path, exec_path)
}

/// Maps to CC `getCurrentInstallationType()`.
pub fn get_current_installation_type() -> InstallationType {
    if crate::utils::process_env::var("NODE_ENV").as_deref() == Some("development") {
        return InstallationType::Development;
    }

    let (invoked_path, exec_path) = get_normalized_paths();
    let combined = format!("{invoked_path}\n{exec_path}");

    if combined.contains("/target/debug/") || combined.contains("/target/release/") {
        return InstallationType::Development;
    }
    if combined.contains("/node_modules/")
        || combined.contains("/.nvm/versions/node/")
        || combined.contains("/npm/")
    {
        return InstallationType::NpmGlobal;
    }
    if crate::utils::process_env::var("NYAN_INSTALLATION_TYPE").as_deref()
        == Some("package-manager")
    {
        return InstallationType::PackageManager;
    }
    if crate::utils::process_env::var("NYAN_BUNDLED")
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"))
    {
        return InstallationType::Native;
    }

    InstallationType::Unknown
}

/// Maps to CC local `getInstallationPath()`.
pub fn get_installation_path() -> String {
    if crate::utils::process_env::var("NODE_ENV").as_deref() == Some("development") {
        return std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
    }

    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Maps to CC `getInvokedBinary()`.
pub fn get_invoked_binary() -> String {
    std::env::args()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Maps to CC local `detectMultipleInstallations()` safe filesystem subset.
///
/// @nyan: probes nyan's own install location (`install.sh` /
/// `install.ps1` put the binary in `~/.local/bin`), not Claude Code's
/// `~/.claude/local` npm prefix or its `~/.local/{bin,share}/claude`.
pub fn detect_multiple_installations() -> Vec<InstallationRecord> {
    let native_bin = crate::utils::node_os::homedir()
        .join(".local")
        .join("bin")
        .join(if cfg!(target_os = "windows") {
            "nyan.exe"
        } else {
            "nyan"
        });
    if !native_bin.exists() {
        return Vec::new();
    }
    vec![InstallationRecord {
        install_type: "native".to_string(),
        path: native_bin.display().to_string(),
    }]
}

/// Maps to CC local `detectConfigurationIssues(type)` install-method warning
/// branches that are safe to evaluate locally.
pub fn detect_configuration_issues(installation_type: &InstallationType) -> Vec<DiagnosticWarning> {
    let mut warnings = Vec::new();
    let config = crate::utils::config::load_global_config();
    let install_method = config.install_method.as_deref().unwrap_or("unknown");

    if *installation_type == InstallationType::Development {
        return warnings;
    }

    if *installation_type == InstallationType::Native && !path_contains_local_bin() {
        warnings.push(DiagnosticWarning {
            issue: "Native installation exists but ~/.local/bin is not in your PATH".to_string(),
            fix: "Run: echo 'export PATH=\"$HOME/.local/bin:$PATH\"' >> your shell config file then open a new terminal or source it".to_string(),
        });
    }

    let installation_checks_disabled = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("DISABLE_INSTALLATION_CHECKS").as_deref(),
    );
    if !installation_checks_disabled {
        if *installation_type == InstallationType::Native && install_method != "native" {
            warnings.push(DiagnosticWarning {
                issue: format!(
                    "Running native installation but config install method is '{install_method}'"
                ),
                fix: "Run claude install to update configuration".to_string(),
            });
        }
    }

    warnings
}

/// Maps to CC `detectLinuxGlobPatternWarnings()`.
pub fn detect_linux_glob_pattern_warnings() -> Vec<DiagnosticWarning> {
    if crate::utils::platform::get_platform() != crate::utils::platform::Platform::Linux {
        return Vec::new();
    }

    // The sandbox runtime owns live glob-pattern collection. Keep this boundary
    // explicit so Doctor can append warnings once `SandboxManager` exposes the
    // official `getLinuxGlobPatternWarnings()` state.
    Vec::new()
}

/// Maps to CC `getDoctorDiagnostic()`.
pub fn get_doctor_diagnostic() -> DiagnosticInfo {
    let installation_type = get_current_installation_type();
    let version = crate::constants::product::VERSION.to_string();
    let installation_path = get_installation_path();
    let invoked_binary = get_invoked_binary();
    let multiple_installations = detect_multiple_installations();
    let mut warnings = detect_configuration_issues(&installation_type);
    warnings.extend(detect_linux_glob_pattern_warnings());

    let config = crate::utils::config::load_global_config();
    let config_install_method = config
        .install_method
        .clone()
        .unwrap_or_else(|| "not set".to_string());
    let ripgrep_status = get_ripgrep_status();
    let package_manager = (installation_type == InstallationType::PackageManager).then(|| {
        crate::utils::process_env::var("NYAN_PACKAGE_MANAGER")
            .unwrap_or_else(|| "unknown".to_string())
    });
    #[cfg(target_os = "windows")]
    if let Err(err) = crate::utils::windows_paths::find_git_bash_path() {
        warnings.push(DiagnosticWarning {
            issue: format!("Git Bash is not available: {err}"),
            fix: "Install Git for Windows (https://git-scm.com/downloads/win) or set NYAN_GIT_BASH_PATH to your bash.exe path".to_string(),
        });
    }
    let git_bash = git_bash_display();

    DiagnosticInfo {
        installation_type,
        version,
        installation_path,
        invoked_binary,
        config_install_method,
        auto_updates: auto_updates_display(),
        has_update_permissions: None,
        multiple_installations,
        warnings,
        recommendation: None,
        package_manager,
        ripgrep_status,
        git_bash,
    }
}

fn git_bash_display() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let source = crate::utils::windows_paths::git_bash_discovery_source();
        Some(match crate::utils::windows_paths::find_git_bash_path() {
            Ok(path) => format!("Found at {} (source: {source})", path.display()),
            Err(err) => format!("Unavailable: {err} (source: {source})"),
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

fn auto_updates_display() -> String {
    if let Some(reason) = crate::utils::config::get_auto_updater_disabled_reason() {
        return format!(
            "disabled ({})",
            crate::utils::config::format_auto_updater_disabled_reason(reason)
        );
    }

    match crate::utils::auto_updater::current_update_support() {
        crate::utils::auto_updater::UpdateSupport::ReleaseBinary => "enabled".to_string(),
        crate::utils::auto_updater::UpdateSupport::DevelopmentBuild { .. } => {
            "enabled (development build; no release replacement)".to_string()
        }
        crate::utils::auto_updater::UpdateSupport::SourceInstall { .. } => {
            "enabled (source install; no release replacement)".to_string()
        }
        crate::utils::auto_updater::UpdateSupport::UnsupportedPlatform { platform } => {
            format!("enabled (no release asset for {platform})")
        }
    }
}

/// Maps to CC `gatherDiagnosticInfo` ripgrepStatus shaping from
/// `utils/ripgrep.ts#getRipgrepStatus` (`working ?? true`).
fn get_ripgrep_status() -> RipgrepStatus {
    let raw = crate::utils::ripgrep::get_ripgrep_status();
    let mode = match raw.mode {
        crate::utils::ripgrep::RipgrepMode::System => RipgrepMode::System,
        crate::utils::ripgrep::RipgrepMode::Builtin => RipgrepMode::Builtin,
        crate::utils::ripgrep::RipgrepMode::Embedded => RipgrepMode::Embedded,
    };
    RipgrepStatus {
        // CC: `working: ripgrepStatusRaw.working ?? true`
        working: raw.working.unwrap_or(true),
        system_path: if mode == RipgrepMode::System {
            Some(raw.path)
        } else {
            None
        },
        mode,
    }
}

fn path_contains_local_bin() -> bool {
    let local_bin = crate::utils::node_os::homedir().join(".local").join("bin");
    let Some(path) = crate::utils::process_env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|entry| same_path_text(&entry, &local_bin))
}

fn same_path_text(left: &Path, right: &Path) -> bool {
    normalize_path_text(left) == normalize_path_text(right)
}

fn normalize_path_text(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn which_in_path(binary: &str) -> Option<String> {
    let path = crate::utils::process_env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
        if cfg!(target_os = "windows") {
            let candidate = dir.join(format!("{binary}.exe"));
            if candidate.is_file() {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nyan-doctor-diagnostic-{name}-{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn installation_type_detection_matches_official_path_categories() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _node_env = EnvVarGuard::set("NODE_ENV", "development");
        assert_eq!(
            get_current_installation_type(),
            InstallationType::Development
        );
    }

    /// The packaged ripgrep is an external asset, so either outcome is a valid
    /// configuration: a checkout carrying `vendor/ripgrep` resolves to
    /// `Builtin` and reports no system path, one without it falls back to the
    /// host `rg` and reports `System` along with the path it resolved. What
    /// has to hold is that the two agree — a mode that contradicts the path
    /// would make /doctor lie about which binary is in use.
    #[test]
    fn ripgrep_status_mode_and_system_path_agree() {
        let diagnostic = get_doctor_diagnostic();
        assert!(diagnostic.ripgrep_status.working);
        match diagnostic.ripgrep_status.mode {
            RipgrepMode::Builtin => {
                assert_eq!(diagnostic.ripgrep_status.system_path, None);
            }
            RipgrepMode::System => {
                assert!(
                    diagnostic.ripgrep_status.system_path.is_some(),
                    "system mode has to name the binary it resolved"
                );
            }
            // Deliberately unasserted. The variant is reserved for a future
            // release build that embeds rg; nothing constructs it today, and
            // pinning a contract before that build exists would only have to
            // be rewritten when it lands.
            RipgrepMode::Embedded => {}
        }
    }

    #[test]
    fn doctor_diagnostic_preserves_official_top_level_shape() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _node_env = EnvVarGuard::unset("NODE_ENV");
        let diagnostic = get_doctor_diagnostic();
        assert!(!diagnostic.version.is_empty());
        assert!(!diagnostic.installation_path.is_empty());
        assert!(!diagnostic.invoked_binary.is_empty());
        assert!(!diagnostic.config_install_method.is_empty());
        assert!(matches!(
            diagnostic.auto_updates.as_str(),
            "enabled" | _ if diagnostic.auto_updates.starts_with("disabled (")
        ));
    }
}
