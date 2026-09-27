//! Maps to: CC `utils/sandbox/sandbox-adapter.ts`.
//!
//! This module owns the `SandboxManager` adapter used by Bash permission,
//! execution, violation annotation, and `/sandbox` settings UI. macOS commands
//! run under Seatbelt and Linux commands under bubblewrap; unavailable backends
//! fail closed rather than falling through to an ordinary shell.

use crate::bootstrap::state::{get_additional_directories_for_claude_md, get_original_cwd};
use crate::tools::bash_tool::tool_name::BASH_TOOL_NAME;
use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
use crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME;
use crate::types::permissions::PermissionUpdate;
use crate::utils::permissions::filesystem::get_claude_temp_dir;
use crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string;
use crate::utils::settings::constants::SettingSource;
use crate::utils::settings::managed_path::get_managed_settings_drop_in_dir;
use crate::utils::settings::types::SettingsJson;
use crate::utils::settings::{
    get_initial_settings, get_settings_file_path_for_source, get_settings_for_source,
    get_settings_root_path_for_source,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

const LINUX_SECCOMP_HELPER_ARG: &str = "__cometix-sandbox-seccomp";

/// Internal Linux apply-seccomp equivalent used after proxy bridge setup.
/// Returns `None` for normal CLI invocations and an exit code if helper setup
/// failed. On success this function `exec`s the requested program.
pub fn run_linux_seccomp_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(LINUX_SECCOMP_HELPER_ARG)) {
        return None;
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("The sandbox seccomp helper is only available on Linux");
        return Some(1);
    }
    #[cfg(target_os = "linux")]
    {
        let Some(program) = args.next() else {
            eprintln!("The sandbox seccomp helper is missing its program");
            return Some(1);
        };
        if let Err(error) = install_unix_socket_seccomp_filter() {
            eprintln!("Could not apply the sandbox Unix-socket filter: {error}");
            return Some(1);
        }
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(program).args(args).exec();
        eprintln!("Could not execute the sandboxed command: {error}");
        Some(1)
    }
}

#[cfg(target_os = "linux")]
fn install_unix_socket_seccomp_filter() -> std::io::Result<()> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
    const ARG0_LOW_OFFSET: u32 = if cfg!(target_endian = "little") {
        16
    } else {
        20
    };

    let mut filters = [
        libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0, // seccomp_data.nr
        },
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 3,
            k: libc::SYS_socket as u32,
        },
        libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: ARG0_LOW_OFFSET,
        },
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 1,
            k: libc::AF_UNIX as u32,
        },
        libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filters.len() as u16,
        filter: filters.as_mut_ptr(),
    };
    let no_new_privileges = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if no_new_privileges != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let filtered = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    };
    if filtered != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Maps to CC `@anthropic-ai/sandbox-runtime` `NetworkHostPattern` as consumed
/// by `SandboxPermissionRequest`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkHostPattern {
    pub host: String,
    pub port: Option<u16>,
}

impl NetworkHostPattern {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: None,
        }
    }

    pub fn with_port(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port: Some(port),
        }
    }
}

/// Maps to: CC `@anthropic-ai/sandbox-runtime` `SandboxAskCallback`.
pub type SandboxAskCallback = Arc<
    dyn Fn(NetworkHostPattern) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

static SANDBOX_ASK_CALLBACK: OnceLock<Mutex<Option<SandboxAskCallback>>> = OnceLock::new();

fn sandbox_ask_callback_slot() -> &'static Mutex<Option<SandboxAskCallback>> {
    SANDBOX_ASK_CALLBACK.get_or_init(|| Mutex::new(None))
}

/// Managed-policy gate applied in front of the registered ask callback.
/// Maps to: CC `utils/sandbox/sandbox-adapter.ts:743-755` — the
/// `wrappedCallback` built inside `initialize`, honoring
/// `shouldAllowManagedSandboxDomainsOnly()` before prompting.
fn wrap_sandbox_ask_callback(callback: SandboxAskCallback) -> SandboxAskCallback {
    Arc::new(move |host_pattern: NetworkHostPattern| {
        let callback = Arc::clone(&callback);
        Box::pin(async move {
            if should_allow_managed_sandbox_domains_only() {
                tracing::debug!(
                    host = %host_pattern.host,
                    "[sandbox] Blocked network request (allowManagedDomainsOnly)"
                );
                return false;
            }
            callback(host_pattern).await
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
    }) as SandboxAskCallback
}

/// RAII ask-callback registration held by the REPL while mounted.
///
/// Maps to: CC `utils/sandbox/sandbox-adapter.ts:729-755` — the ask-callback
/// registration slice of `SandboxManager.initialize(sandboxAskCallback)`
/// (REPL render-body call site: REPL.tsx:3087-3094). L1: PORTING.md
/// `Mount-scoped external callback registration guard`. Runtime/bwrap init remains
/// short-circuited; this stores the REPL ask seam so network-host prompts can
/// invoke it. CC's process-lifetime REPL never unregisters; Cometix clears the
/// slot on REPL unmount (iocraft Drop-guard precedent:
/// `state/overlay.rs#OverlayRegistration`) so a producer thread parked on an
/// ask denies (`recv` Err → `false`) instead of hanging on a stale entry.
/// Seam: CC has a second registrant (`cli/print.ts:620`); the Rust headless
/// path has no registrant today — route it through `register` when ported.
pub struct SandboxAskCallbackRegistration {
    registered: Option<SandboxAskCallback>,
}

impl SandboxAskCallbackRegistration {
    /// Install `callback` behind CC's `isSandboxingEnabled()` gate
    /// (REPL.tsx:3087). Disabled sandboxing registers nothing.
    /// Seam (recorded): CC re-runs this gate every render, so enabling
    /// sandboxing mid-session installs the callback on the next render; the
    /// REPL calls `register` once at mount, so a mid-session enable does not
    /// install it until the next REPL mount.
    pub fn register(callback: SandboxAskCallback) -> Self {
        if !is_sandboxing_enabled() {
            return Self { registered: None };
        }
        let wrapped = wrap_sandbox_ask_callback(callback);
        *sandbox_ask_callback_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&wrapped));
        Self {
            registered: Some(wrapped),
        }
    }
}

impl Drop for SandboxAskCallbackRegistration {
    fn drop(&mut self) {
        let Some(mine) = self.registered.take() else {
            return;
        };
        let mut slot = sandbox_ask_callback_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Only clear our own registration: a successor REPL may have
        // re-registered before this guard dropped.
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &mine))
        {
            *slot = None;
        }
    }
}

/// Clear the registered REPL ask callback between tests.
#[cfg(test)]
pub fn clear_sandbox_ask_callback_for_test() {
    *sandbox_ask_callback_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

pub async fn ask_for_network_host_permission(host_pattern: NetworkHostPattern) -> bool {
    let callback = sandbox_ask_callback_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match callback {
        Some(callback) => callback(host_pattern).await,
        None => false,
    }
}

/// Maps to CC `@anthropic-ai/sandbox-runtime` `SandboxDependencyCheck`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxDependencyCheck {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Maps to CC `SandboxManager.checkDependencies()`.
pub fn check_dependencies_readonly() -> SandboxDependencyCheck {
    let mut errors = Vec::new();
    if !command_exists("rg") {
        errors.push("ripgrep (rg) not found".to_string());
    }
    if cfg!(target_os = "linux") {
        if !command_exists("bwrap") {
            errors.push("bwrap not installed".to_string());
        }
        if !command_exists("socat") {
            errors.push("socat not installed".to_string());
        }
    } else if cfg!(target_os = "macos") && !Path::new("/usr/bin/sandbox-exec").is_file() {
        errors.push("sandbox-exec not installed".to_string());
    }
    SandboxDependencyCheck {
        errors,
        warnings: Vec::new(),
    }
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(command).is_file()))
}

/// Maps to: CC `utils/sandbox/sandbox-adapter.ts:491-493` `isSupportedPlatform`.
pub fn is_supported_platform() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Maps to: CC `utils/sandbox/sandbox-adapter.ts:505-526` `isPlatformInEnabledList`.
pub fn is_platform_in_enabled_list() -> bool {
    let settings = get_initial_settings();
    let Some(enabled_platforms) = settings
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.get("enabledPlatforms"))
    else {
        return true;
    };
    let Some(enabled_platforms) = enabled_platforms.as_array() else {
        // The source catches malformed settings and defaults to enabled.
        return true;
    };
    if enabled_platforms.is_empty() {
        return false;
    }

    let current_platform = if crate::utils::env::is_wsl() {
        "wsl"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        std::env::consts::OS
    };
    enabled_platforms
        .iter()
        .any(|platform| platform.as_str() == Some(current_platform))
}

/// Maps to: CC `utils/sandbox/sandbox-adapter.ts:528-546` `isSandboxingEnabled`.
pub fn is_sandboxing_enabled() -> bool {
    if !is_supported_platform() {
        return false;
    }
    if !check_dependencies_readonly().errors.is_empty() {
        return false;
    }
    if !is_platform_in_enabled_list() {
        return false;
    }
    get_sandbox_enabled_setting(&get_initial_settings())
}

/// Maps to CC `utils/sandbox/sandbox-adapter.ts:798-803#refreshConfig`.
/// Seatbelt/bwrap filesystem profiles are compiled from the latest settings
/// and additional directories at each wrap_shell_command. Only the persistent
/// network proxy retains policy; refresh it synchronously before returning.
pub fn refresh_config() {
    if !is_sandboxing_enabled() {
        return;
    }
    let config = convert_to_sandbox_runtime_config(&get_initial_settings());
    super::network_proxy::update_network_config(&config.network);
}

/// An OS-enforced shell invocation produced by `SandboxManager.wrapWithSandbox`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxedShellCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Missing bare-repository markers captured before spawn and scrubbed as
    /// soon as the sandboxed process exits.
    pub cleanup_paths: Vec<PathBuf>,
}

/// Maps to CC `SandboxManager.wrapWithSandbox(command, binShell, ...)`.
///
/// macOS uses Seatbelt (`sandbox-exec`) and Linux uses bubblewrap. Both paths
/// enforce filesystem write restrictions and deny direct network access. The
/// latter is deliberately fail-closed until the official domain proxy is
/// available: a command that needs network can request an unsandboxed retry.
pub fn wrap_shell_command(
    shell: &str,
    command: &str,
    cwd: &Path,
    settings: &SettingsJson,
) -> Result<SandboxedShellCommand, String> {
    let mut config = convert_to_sandbox_runtime_config(settings);
    let cleanup_paths = protect_against_bare_git_repo_planting(cwd, &mut config);
    let network = super::network_proxy::ensure_network_proxy(&config.network)
        .map_err(|error| format!("Sandbox network proxy initialization failed: {error:#}"))?;
    let mut wrapped = if cfg!(target_os = "macos") {
        wrap_shell_command_macos(shell, command, cwd, &config, &network)?
    } else if cfg!(target_os = "linux") {
        wrap_shell_command_linux(shell, command, cwd, &config, &network)?
    } else {
        return Err(format!(
            "Sandbox execution is not supported on {}",
            std::env::consts::OS
        ));
    };
    wrapped.cleanup_paths = cleanup_paths;
    Ok(wrapped)
}

fn protect_against_bare_git_repo_planting(
    cwd: &Path,
    config: &mut SandboxRuntimeConfig,
) -> Vec<PathBuf> {
    const BARE_GIT_REPO_FILES: &[&str] = &["HEAD", "objects", "refs", "hooks", "config"];
    let original = crate::bootstrap::state::get_original_cwd();
    let mut directories = vec![original];
    if !directories.iter().any(|directory| directory == cwd) {
        directories.push(cwd.to_path_buf());
    }
    let mut cleanup_paths = Vec::new();
    for directory in directories {
        for name in BARE_GIT_REPO_FILES {
            let path = directory.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => config
                    .filesystem
                    .deny_write
                    .push(path.display().to_string()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    cleanup_paths.push(path)
                }
                Err(_) => config
                    .filesystem
                    .deny_write
                    .push(path.display().to_string()),
            }
        }
    }
    if let Some(main_repo) = detect_worktree_main_repo(cwd) {
        if main_repo != cwd {
            config
                .filesystem
                .allow_write
                .push(main_repo.display().to_string());
        }
    }
    config.filesystem.deny_write.sort();
    config.filesystem.deny_write.dedup();
    cleanup_paths.sort();
    cleanup_paths.dedup();
    cleanup_paths
}

fn detect_worktree_main_repo(cwd: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(cwd.join(".git")).ok()?;
    let gitdir = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:").map(str::trim))?;
    let gitdir = {
        let path = PathBuf::from(gitdir);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let components = gitdir.components().collect::<Vec<_>>();
    let marker = components
        .windows(2)
        .rposition(|pair| pair[0].as_os_str() == ".git" && pair[1].as_os_str() == "worktrees")?;
    let mut result = PathBuf::new();
    for component in &components[..marker] {
        result.push(component.as_os_str());
    }
    (!result.as_os_str().is_empty()).then_some(result)
}

/// Maps to CC `SandboxManager.cleanupAfterCommand()`'s bare-repo scrub.
pub fn cleanup_after_command(paths: &[PathBuf]) {
    for path in paths {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            continue;
        };
        let result = if metadata.is_dir() && !metadata.file_type().is_symlink() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if result.is_ok() {
            crate::utils::debug::log_for_debugging(&format!(
                "[Sandbox] scrubbed planted bare-repo file: {}",
                path.display()
            ));
        }
    }
}

fn sandbox_path(pattern: &str, cwd: &Path) -> PathBuf {
    let expanded = if pattern == "." {
        cwd.display().to_string()
    } else {
        crate::utils::permissions::path_validation::expand_tilde(pattern)
    };
    let path = PathBuf::from(expanded);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    // Runtime backends do not understand globs. Restrict the static prefix;
    // this is intentionally broader (and therefore safer for deny rules).
    let text = path.display().to_string();
    let first_glob = text
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '*' | '?' | '[' | '{').then_some(index));
    let path = match first_glob {
        Some(index) => {
            let prefix = &text[..index];
            let separator = prefix.rfind(['/', '\\']).unwrap_or(0);
            PathBuf::from(if separator == 0 {
                "/"
            } else {
                &prefix[..separator]
            })
        }
        None => path,
    };
    canonicalize_existing_prefix(&path)
}

fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut cursor = path;
    let mut suffix = Vec::new();
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(mut canonical) = parent.canonicalize() {
            for component in suffix.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut cursor = path;
    loop {
        if cursor.exists() {
            return cursor
                .canonicalize()
                .unwrap_or_else(|_| cursor.to_path_buf());
        }
        let Some(parent) = cursor.parent() else {
            return PathBuf::from("/");
        };
        cursor = parent;
    }
}

fn sandbox_paths(patterns: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut paths = patterns
        .iter()
        .map(|pattern| sandbox_path(pattern, cwd))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn sandbox_allow_write_paths(patterns: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for pattern in patterns {
        let expanded = if pattern == "." {
            cwd.to_path_buf()
        } else {
            let expanded = crate::utils::permissions::path_validation::expand_tilde(pattern);
            let path = PathBuf::from(expanded);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        };
        let canonical = canonicalize_existing_prefix(&expanded);
        let original = expanded.components().collect::<PathBuf>();
        let original_text = original.display().to_string();
        let canonical_text = canonical.display().to_string();
        let macos_system_alias = (original_text.starts_with("/tmp/")
            || original_text.starts_with("/var/"))
            && canonical_text == format!("/private{original_text}");
        let same_or_descendant = canonical == original || canonical.starts_with(&original);
        if same_or_descendant || macos_system_alias || pattern == "." {
            paths.push(canonical);
        } else {
            crate::utils::debug::log_for_debugging(&format!(
                "[Sandbox] skipped write path whose symlink escapes its boundary: {} -> {}",
                original.display(),
                canonical.display()
            ));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn macos_base_profile(enable_weaker_network_isolation: bool) -> String {
    // Security-relevant base of sandbox-runtime `generateSandboxProfile`.
    // In particular, this is deny-by-default and does not grant wildcard Mach
    // lookup; `trustd.agent` is available only behind the explicit weaker gate.
    let mut profile = String::from(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow process-info* (target same-sandbox))
(allow signal (target same-sandbox))
(allow mach-priv-task-port (target same-sandbox))
(allow user-preference-read)
(allow mach-lookup
  (global-name "com.apple.audio.systemsoundserver")
  (global-name "com.apple.distributed_notifications@Uv3")
  (global-name "com.apple.FontObjectsServer")
  (global-name "com.apple.fonts")
  (global-name "com.apple.logd")
  (global-name "com.apple.lsd.mapdb")
  (global-name "com.apple.PowerManagement.control")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.securityd.xpc")
  (global-name "com.apple.coreservices.launchservicesd")
  (global-name "com.apple.SecurityServer"))
(allow ipc-posix-shm)
(allow ipc-posix-sem)
(allow iokit-open
  (iokit-registry-entry-class "IOSurfaceRootUserClient")
  (iokit-registry-entry-class "RootDomainUserClient")
  (iokit-user-client-class "IOSurfaceSendRight"))
(allow iokit-get-properties)
(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))
(allow sysctl-read)
(allow sysctl-write (sysctl-name "kern.tcsm_enable"))
(allow distributed-notification-post)
(allow file-read*)
(allow file-map-executable)
(allow file-ioctl (literal "/dev/null"))
(allow file-ioctl (literal "/dev/zero"))
(allow file-ioctl (literal "/dev/random"))
(allow file-ioctl (literal "/dev/urandom"))
(allow file-ioctl (literal "/dev/dtracehelper"))
(allow file-ioctl (literal "/dev/tty"))
(allow file-ioctl file-read-data file-write-data
  (require-all (literal "/dev/null") (vnode-type CHARACTER-DEVICE)))
"#,
    );
    if enable_weaker_network_isolation {
        profile.push_str("(allow mach-lookup (global-name \"com.apple.trustd.agent\"))\n");
    }
    profile
}

fn append_macos_mandatory_write_denies(profile: &mut String) {
    // Maps to sandbox-runtime `macGetMandatoryDenyPatterns()`.
    profile.push_str(
        "(deny file-write* (regex #\"(^|/)\\.(gitconfig|gitmodules|bashrc|bash_profile|zshrc|zprofile|profile|ripgreprc|mcp\\.json)$\"))\n",
    );
    profile.push_str("(deny file-write* (regex #\"(^|/)\\.(vscode|idea)(/|$)\"))\n");
    profile.push_str("(deny file-write* (regex #\"(^|/)\\.claude/(commands|agents)(/|$)\"))\n");
    profile.push_str("(deny file-write* (regex #\"(^|/)\\.git/hooks(/|$)\"))\n");
    profile.push_str("(deny file-write* (regex #\"(^|/)\\.git/config$\"))\n");
}

fn linux_existing_mandatory_write_denies(cwd: &Path) -> Vec<PathBuf> {
    const DANGEROUS_FILES: &[&str] = &[
        ".gitconfig",
        ".gitmodules",
        ".bashrc",
        ".bash_profile",
        ".zshrc",
        ".zprofile",
        ".profile",
        ".ripgreprc",
        ".mcp.json",
    ];
    fn walk(directory: &Path, depth: usize, output: &mut Vec<PathBuf>, visited: &mut usize) {
        if depth > 3 || *visited >= 100_000 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            *visited += 1;
            if *visited >= 100_000 {
                return;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if metadata.is_file() {
                if DANGEROUS_FILES.iter().any(|dangerous| *dangerous == name)
                    || (name == "config"
                        && path.parent().is_some_and(|parent| {
                            parent.file_name().is_some_and(|name| name == ".git")
                        }))
                {
                    output.push(path);
                }
                continue;
            }
            if !metadata.is_dir() {
                continue;
            }
            let protected = matches!(name.as_ref(), ".vscode" | ".idea")
                || (matches!(name.as_ref(), "commands" | "agents")
                    && path.parent().is_some_and(|parent| {
                        parent.file_name().is_some_and(|name| name == ".claude")
                    }))
                || (name == "hooks"
                    && path.parent().is_some_and(|parent| {
                        parent.file_name().is_some_and(|name| name == ".git")
                    }));
            if protected {
                output.push(path);
            } else if name != "node_modules" && name != ".git" {
                walk(&path, depth + 1, output, visited);
            } else if name == ".git" {
                for child in ["hooks", "config"] {
                    let child = path.join(child);
                    if child.exists() {
                        output.push(child);
                    }
                }
            }
        }
    }
    let mut output = Vec::new();
    let mut visited = 0;
    walk(cwd, 0, &mut output, &mut visited);
    output.sort();
    output.dedup();
    output
}

fn seatbelt_quote(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn wrap_shell_command_macos(
    shell: &str,
    command: &str,
    cwd: &Path,
    config: &SandboxRuntimeConfig,
    network: &super::network_proxy::NetworkProxyEndpoints,
) -> Result<SandboxedShellCommand, String> {
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err("Sandbox is enabled but /usr/bin/sandbox-exec is unavailable".to_string());
    }
    let mut profile = macos_base_profile(config.enable_weaker_network_isolation == Some(true));
    if config.network.allow_local_binding == Some(true) {
        profile.push_str("(allow network-bind (local ip \"*:*\"))\n");
        profile.push_str("(allow network-inbound (local ip \"*:*\"))\n");
        profile.push_str("(allow network-outbound (local ip \"*:*\"))\n");
    }
    if config.network.allow_all_unix_sockets == Some(true) {
        profile.push_str("(allow system-socket (socket-domain AF_UNIX))\n");
        profile.push_str("(allow network-bind (local unix-socket (path-regex #\"^/\")))\n");
        profile.push_str("(allow network-outbound (remote unix-socket (path-regex #\"^/\")))\n");
    } else if let Some(paths) = config.network.allow_unix_sockets.as_ref() {
        if !paths.is_empty() {
            profile.push_str("(allow system-socket (socket-domain AF_UNIX))\n");
        }
        for path in sandbox_paths(paths, cwd) {
            profile.push_str(&format!(
                "(allow network-bind (local unix-socket (subpath \"{}\")))\n",
                seatbelt_quote(&path)
            ));
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (subpath \"{}\")))\n",
                seatbelt_quote(&path)
            ));
        }
    }
    for port in [network.http_port, network.socks_port] {
        profile.push_str(&format!(
            "(allow network-bind (local ip \"localhost:{port}\"))\n"
        ));
        profile.push_str(&format!(
            "(allow network-inbound (local ip \"localhost:{port}\"))\n"
        ));
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    let mut allow_write = sandbox_allow_write_paths(&config.filesystem.allow_write, cwd);
    allow_write.push(canonicalize_existing_prefix(cwd));
    allow_write.sort();
    allow_write.dedup();
    for path in &allow_write {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            seatbelt_quote(path)
        ));
    }
    for path in [
        "/dev/null",
        "/dev/tty",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/dtracehelper",
        "/dev/autofs_nowait",
    ] {
        profile.push_str(&format!("(allow file-write* (literal \"{path}\"))\n"));
    }
    for path in sandbox_paths(&config.filesystem.deny_write, cwd) {
        profile.push_str(&format!(
            "(deny file-write* (subpath \"{}\"))\n",
            seatbelt_quote(&path)
        ));
    }
    append_macos_mandatory_write_denies(&mut profile);
    let allow_read = sandbox_paths(&config.filesystem.allow_read, cwd);
    for path in sandbox_paths(&config.filesystem.deny_read, cwd) {
        if allow_read.is_empty() {
            profile.push_str(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                seatbelt_quote(&path)
            ));
        } else {
            profile.push_str(&format!(
                "(deny file-read* (require-all (subpath \"{}\")",
                seatbelt_quote(&path)
            ));
            for allowed in &allow_read {
                profile.push_str(&format!(
                    " (require-not (subpath \"{}\"))",
                    seatbelt_quote(allowed)
                ));
            }
            profile.push_str("))\n");
        }
    }
    let mut args = proxy_environment(network.http_port, network.socks_port);
    args.extend([
        "/usr/bin/sandbox-exec".to_string(),
        "-p".to_string(),
        profile,
        shell.to_string(),
        "-c".to_string(),
        command.to_string(),
    ]);
    Ok(SandboxedShellCommand {
        program: "/usr/bin/env".to_string(),
        args,
        cleanup_paths: Vec::new(),
    })
}

fn proxy_environment(http_port: u16, socks_port: u16) -> Vec<String> {
    let tmpdir = std::env::var("CLAUDE_TMPDIR").unwrap_or_else(|_| {
        crate::utils::permissions::filesystem::get_claude_temp_dir()
            .display()
            .to_string()
    });
    let no_proxy = "localhost,127.0.0.1,::1,*.local,.local,169.254.0.0/16,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";
    vec![
        "SANDBOX_RUNTIME=1".to_string(),
        format!("TMPDIR={tmpdir}"),
        format!("NO_PROXY={no_proxy}"),
        format!("no_proxy={no_proxy}"),
        format!("HTTP_PROXY=http://localhost:{http_port}"),
        format!("HTTPS_PROXY=http://localhost:{http_port}"),
        format!("http_proxy=http://localhost:{http_port}"),
        format!("https_proxy=http://localhost:{http_port}"),
        format!("ALL_PROXY=socks5h://localhost:{socks_port}"),
        format!("all_proxy=socks5h://localhost:{socks_port}"),
        format!("FTP_PROXY=socks5h://localhost:{socks_port}"),
        format!("ftp_proxy=socks5h://localhost:{socks_port}"),
        format!("RSYNC_PROXY=localhost:{socks_port}"),
        format!("DOCKER_HTTP_PROXY=http://localhost:{http_port}"),
        format!("DOCKER_HTTPS_PROXY=http://localhost:{http_port}"),
    ]
}

fn wrap_shell_command_linux(
    shell: &str,
    command: &str,
    cwd: &Path,
    config: &SandboxRuntimeConfig,
    network: &super::network_proxy::NetworkProxyEndpoints,
) -> Result<SandboxedShellCommand, String> {
    if !command_exists("bwrap") {
        return Err("Sandbox is enabled but bwrap is unavailable".to_string());
    }
    let mut args = vec![
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--unshare-net".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
    ];
    let mut allow_write = sandbox_allow_write_paths(&config.filesystem.allow_write, cwd);
    allow_write.push(canonicalize_existing_prefix(cwd));
    allow_write.sort();
    allow_write.dedup();
    for path in allow_write.into_iter().filter(|path| path.exists()) {
        let path = path.display().to_string();
        args.extend(["--bind".to_string(), path.clone(), path]);
    }
    let mut deny_write = sandbox_paths(&config.filesystem.deny_write, cwd);
    deny_write.extend(linux_existing_mandatory_write_denies(cwd));
    deny_write.sort();
    deny_write.dedup();
    for path in deny_write {
        let protected = if path.exists() {
            path
        } else {
            nearest_existing_ancestor(path.parent().unwrap_or_else(|| Path::new("/")))
        };
        let protected = protected.display().to_string();
        args.extend(["--ro-bind".to_string(), protected.clone(), protected]);
    }
    for path in sandbox_paths(&config.filesystem.deny_read, cwd) {
        if path.exists() {
            let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
            let path = path.display().to_string();
            if metadata.is_dir() {
                args.extend(["--tmpfs".to_string(), path]);
            } else {
                args.extend(["--ro-bind".to_string(), "/dev/null".to_string(), path]);
            }
        } else {
            // bwrap requires mount destinations to exist. Protect the nearest
            // host ancestor read-only rather than planting a host placeholder
            // that could survive the command as a fake HEAD/.bashrc file.
            let parent = nearest_existing_ancestor(path.parent().unwrap_or_else(|| Path::new("/")));
            let parent = parent.display().to_string();
            args.extend(["--ro-bind".to_string(), parent.clone(), parent]);
        }
    }
    // Bind explicit read exceptions last so they can pierce a broader tmpfs
    // deny mount, matching runtime allowRead precedence.
    for path in sandbox_paths(&config.filesystem.allow_read, cwd)
        .into_iter()
        .filter(|path| path.exists())
    {
        let path = path.display().to_string();
        args.extend(["--ro-bind".to_string(), path.clone(), path]);
    }

    let http_socket = network
        .linux_http_socket
        .as_ref()
        .filter(|path| path.exists())
        .ok_or_else(|| "Linux sandbox HTTP bridge socket is unavailable".to_string())?;
    let socks_socket = network
        .linux_socks_socket
        .as_ref()
        .filter(|path| path.exists())
        .ok_or_else(|| "Linux sandbox SOCKS bridge socket is unavailable".to_string())?;
    for socket in [http_socket, socks_socket] {
        let socket = socket.display().to_string();
        args.extend(["--bind".to_string(), socket.clone(), socket]);
    }
    for variable in proxy_environment(3128, 1080) {
        let (key, value) = variable
            .split_once('=')
            .ok_or_else(|| "invalid sandbox proxy environment variable".to_string())?;
        args.extend(["--setenv".to_string(), key.to_string(), value.to_string()]);
    }
    args.extend(["--dev".to_string(), "/dev".to_string()]);
    args.push("--unshare-pid".to_string());
    if config.enable_weaker_nested_sandbox != Some(true) {
        args.extend(["--proc".to_string(), "/proc".to_string()]);
    }
    args.extend([
        "--setenv".to_string(),
        "CLAUDE_CODE_HOST_HTTP_PROXY_PORT".to_string(),
        network.http_port.to_string(),
        "--setenv".to_string(),
        "CLAUDE_CODE_HOST_SOCKS_PROXY_PORT".to_string(),
        network.socks_port.to_string(),
        "--setenv".to_string(),
        "GIT_SSH_COMMAND".to_string(),
        "ssh -o ProxyCommand='socat - PROXY:localhost:%h:%p,proxyport=3128'".to_string(),
    ]);

    let http_socket_text = http_socket.display().to_string();
    let socks_socket_text = socks_socket.display().to_string();
    let quoted_http = crate::utils::bash::shell_quote::quote(&[&http_socket_text]);
    let quoted_socks = crate::utils::bash::shell_quote::quote(&[&socks_socket_text]);
    let user_command = if config.network.allow_all_unix_sockets == Some(true) {
        let quoted_command = crate::utils::bash::shell_quote::quote(&[command]);
        format!("eval {quoted_command}")
    } else {
        let helper = std::env::current_exe()
            .map_err(|error| format!("Could not resolve seccomp helper executable: {error}"))?
            .display()
            .to_string();
        crate::utils::bash::shell_quote::quote(&[
            &helper,
            LINUX_SECCOMP_HELPER_ARG,
            shell,
            "-c",
            command,
        ])
    };
    let sandbox_command = format!(
        "socat TCP-LISTEN:3128,fork,reuseaddr UNIX-CONNECT:{quoted_http} >/dev/null 2>&1 &\nhttp_proxy_pid=$!\nsocat TCP-LISTEN:1080,fork,reuseaddr UNIX-CONNECT:{quoted_socks} >/dev/null 2>&1 &\nsocks_proxy_pid=$!\ncleanup_proxy_bridges() {{ kill \"$http_proxy_pid\" \"$socks_proxy_pid\" 2>/dev/null || true; }}\ntrap cleanup_proxy_bridges EXIT\n{user_command}"
    );
    args.extend([
        "--chdir".to_string(),
        cwd.display().to_string(),
        "--".to_string(),
        shell.to_string(),
        "-c".to_string(),
        sandbox_command,
    ]);
    Ok(SandboxedShellCommand {
        program: "bwrap".to_string(),
        args,
        cleanup_paths: Vec::new(),
    })
}

/// Maps to CC `@anthropic-ai/sandbox-runtime` `NetworkRestrictionConfig` as
/// produced by CC `convertToSandboxRuntimeConfig(...)`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRestrictionConfig {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Option<Vec<String>>,
    pub allow_all_unix_sockets: Option<bool>,
    pub allow_local_binding: Option<bool>,
    pub http_proxy_port: Option<u16>,
    pub socks_proxy_port: Option<u16>,
}

/// Maps to CC `@anthropic-ai/sandbox-runtime` filesystem config as produced by
/// CC `convertToSandboxRuntimeConfig(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxFilesystemConfig {
    pub deny_read: Vec<String>,
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
}

/// Maps to CC `SandboxRuntimeConfig` from `@anthropic-ai/sandbox-runtime`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRuntimeConfig {
    pub network: NetworkRestrictionConfig,
    pub filesystem: SandboxFilesystemConfig,
    pub ignore_violations: Option<serde_json::Value>,
    pub enable_weaker_nested_sandbox: Option<bool>,
    pub enable_weaker_network_isolation: Option<bool>,
    /// The real CC adapter supplies the bundled ripgrep command when this is
    /// absent from settings. Cometix has not ported that runtime dependency
    /// yet, so this safe config projection only preserves explicit settings.
    pub ripgrep: Option<serde_json::Value>,
}

/// Maps to: CC local `permissionRuleExtractPrefix(...)`.
pub fn permission_rule_extract_prefix(permission_rule: &str) -> Option<String> {
    permission_rule
        .strip_suffix(":*")
        .filter(|prefix| !prefix.is_empty())
        .map(ToString::to_string)
}

/// Maps to: CC `resolvePathPatternForSandbox(...)`.
pub fn resolve_path_pattern_for_sandbox(pattern: &str, source: SettingSource) -> String {
    if let Some(rest) = pattern.strip_prefix("//") {
        return format!("/{rest}");
    }

    if pattern.starts_with('/') {
        return normalize_path_string(
            get_settings_root_path_for_source(source).join(&pattern[1..]),
        );
    }

    pattern.to_string()
}

/// Maps to: CC `resolveSandboxFilesystemPath(...)`.
pub fn resolve_sandbox_filesystem_path(pattern: &str, source: SettingSource) -> String {
    if let Some(rest) = pattern.strip_prefix("//") {
        return format!("/{rest}");
    }

    expand_path(pattern, &get_settings_root_path_for_source(source))
}

fn sandbox_bool(settings: Option<&SettingsJson>, pointer: &str) -> Option<bool> {
    settings
        .and_then(|settings| settings.sandbox.as_ref())
        .and_then(|sandbox| sandbox.pointer(pointer))
        .and_then(serde_json::Value::as_bool)
}

fn sandbox_string_array(settings: Option<&SettingsJson>, pointer: &str) -> Vec<String> {
    settings
        .and_then(|settings| settings.sandbox.as_ref())
        .and_then(|sandbox| sandbox.pointer(pointer))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn sandbox_u16(settings: Option<&SettingsJson>, pointer: &str) -> Option<u16> {
    settings
        .and_then(|settings| settings.sandbox.as_ref())
        .and_then(|sandbox| sandbox.pointer(pointer))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

fn sandbox_value(settings: Option<&SettingsJson>, pointer: &str) -> Option<serde_json::Value> {
    settings
        .and_then(|settings| settings.sandbox.as_ref())
        .and_then(|sandbox| sandbox.pointer(pointer))
        .cloned()
}

fn source_order() -> [SettingSource; 5] {
    [
        SettingSource::User,
        SettingSource::Project,
        SettingSource::Local,
        SettingSource::Flag,
        SettingSource::Policy,
    ]
}

fn push_web_fetch_domain_rules(target: &mut Vec<String>, rules: Option<&Vec<String>>) {
    for rule_string in rules.into_iter().flatten() {
        let rule = permission_rule_value_from_string(rule_string);
        if rule.tool_name == WEB_FETCH_TOOL_NAME {
            if let Some(domain) = rule
                .rule_content
                .as_deref()
                .and_then(|content| content.strip_prefix("domain:"))
            {
                target.push(domain.to_string());
            }
        }
    }
}

fn push_file_permission_paths(
    allow_write: &mut Vec<String>,
    deny_write: &mut Vec<String>,
    deny_read: &mut Vec<String>,
    source: SettingSource,
    settings: &SettingsJson,
) {
    let Some(permissions) = settings.permissions.as_ref() else {
        return;
    };

    for rule_string in permissions.allow.iter().flatten() {
        let rule = permission_rule_value_from_string(rule_string);
        if rule.tool_name == "Edit" {
            if let Some(content) = rule.rule_content.as_deref() {
                allow_write.push(resolve_path_pattern_for_sandbox(content, source));
            }
        }
    }

    for rule_string in permissions.deny.iter().flatten() {
        let rule = permission_rule_value_from_string(rule_string);
        if rule.tool_name == "Edit" {
            if let Some(content) = rule.rule_content.as_deref() {
                deny_write.push(resolve_path_pattern_for_sandbox(content, source));
            }
        }
        if rule.tool_name == FILE_READ_TOOL_NAME {
            if let Some(content) = rule.rule_content.as_deref() {
                deny_read.push(resolve_path_pattern_for_sandbox(content, source));
            }
        }
    }
}

fn push_sandbox_filesystem_paths(
    allow_write: &mut Vec<String>,
    deny_write: &mut Vec<String>,
    deny_read: &mut Vec<String>,
    allow_read: &mut Vec<String>,
    source: SettingSource,
    settings: &SettingsJson,
    managed_read_paths_only: bool,
) {
    for path in sandbox_string_array(Some(settings), "/filesystem/allowWrite") {
        allow_write.push(resolve_sandbox_filesystem_path(&path, source));
    }
    for path in sandbox_string_array(Some(settings), "/filesystem/denyWrite") {
        deny_write.push(resolve_sandbox_filesystem_path(&path, source));
    }
    for path in sandbox_string_array(Some(settings), "/filesystem/denyRead") {
        deny_read.push(resolve_sandbox_filesystem_path(&path, source));
    }
    if !managed_read_paths_only || source == SettingSource::Policy {
        for path in sandbox_string_array(Some(settings), "/filesystem/allowRead") {
            allow_read.push(resolve_sandbox_filesystem_path(&path, source));
        }
    }
}

/// Maps to: CC `shouldAllowManagedSandboxDomainsOnly()`.
///
/// Current safety behavior: reads only file-based managed settings via the
/// existing settings loader. Registry/MDM policy sources and sandbox-runtime
/// enforcement are not executed here.
pub fn should_allow_managed_sandbox_domains_only() -> bool {
    sandbox_bool(
        get_settings_for_source(SettingSource::Policy).as_ref(),
        "/network/allowManagedDomainsOnly",
    )
    .unwrap_or(false)
}

/// Maps to: CC `shouldAllowManagedReadPathsOnly()`.
pub fn should_allow_managed_read_paths_only() -> bool {
    sandbox_bool(
        get_settings_for_source(SettingSource::Policy).as_ref(),
        "/filesystem/allowManagedReadPathsOnly",
    )
    .unwrap_or(false)
}

/// Maps to: CC `convertToSandboxRuntimeConfig(...)`.
///
/// This builds the same settings-derived config shape without invoking
/// `@anthropic-ai/sandbox-runtime` or starting OS sandboxing. Runtime-only
/// pieces (dependency checks, worktree probing, bare-repo scrubbing, bundled
/// ripgrep fallback, and settings subscriptions) remain deferred.
pub fn convert_to_sandbox_runtime_config(settings: &SettingsJson) -> SandboxRuntimeConfig {
    let policy_settings = get_settings_for_source(SettingSource::Policy);
    let managed_domains_only = should_allow_managed_sandbox_domains_only();
    let managed_read_paths_only = should_allow_managed_read_paths_only();

    let mut allowed_domains = Vec::new();
    if managed_domains_only {
        allowed_domains.extend(sandbox_string_array(
            policy_settings.as_ref(),
            "/network/allowedDomains",
        ));
        push_web_fetch_domain_rules(
            &mut allowed_domains,
            policy_settings
                .as_ref()
                .and_then(|settings| settings.permissions.as_ref())
                .and_then(|permissions| permissions.allow.as_ref()),
        );
    } else {
        allowed_domains.extend(sandbox_string_array(
            Some(settings),
            "/network/allowedDomains",
        ));
        push_web_fetch_domain_rules(
            &mut allowed_domains,
            settings
                .permissions
                .as_ref()
                .and_then(|permissions| permissions.allow.as_ref()),
        );
    }

    let mut denied_domains = Vec::new();
    push_web_fetch_domain_rules(
        &mut denied_domains,
        settings
            .permissions
            .as_ref()
            .and_then(|permissions| permissions.deny.as_ref()),
    );

    let mut allow_write = vec![".".to_string(), get_claude_temp_dir().display().to_string()];
    let mut deny_write = Vec::new();
    let mut deny_read = Vec::new();
    let mut allow_read = Vec::new();

    for source in source_order() {
        if let Some(path) = get_settings_file_path_for_source(source) {
            deny_write.push(path.display().to_string());
        }
    }
    deny_write.push(get_managed_settings_drop_in_dir().display().to_string());

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let original_cwd = get_original_cwd();
    if cwd != original_cwd {
        deny_write.push(cwd.join(".claude/settings.json").display().to_string());
        deny_write.push(
            cwd.join(".claude/settings.local.json")
                .display()
                .to_string(),
        );
    }

    deny_write.push(original_cwd.join(".claude/skills").display().to_string());
    if cwd != original_cwd {
        deny_write.push(cwd.join(".claude/skills").display().to_string());
    }

    if let Some(additional) = settings
        .permissions
        .as_ref()
        .and_then(|permissions| permissions.additional_directories.as_ref())
    {
        allow_write.extend(additional.iter().cloned());
    }
    allow_write.extend(
        get_additional_directories_for_claude_md()
            .into_iter()
            .map(|path| path.display().to_string()),
    );

    for source in source_order() {
        if let Some(source_settings) = get_settings_for_source(source) {
            push_file_permission_paths(
                &mut allow_write,
                &mut deny_write,
                &mut deny_read,
                source,
                &source_settings,
            );
            push_sandbox_filesystem_paths(
                &mut allow_write,
                &mut deny_write,
                &mut deny_read,
                &mut allow_read,
                source,
                &source_settings,
                managed_read_paths_only,
            );
        }
    }

    SandboxRuntimeConfig {
        network: NetworkRestrictionConfig {
            allowed_domains,
            denied_domains,
            allow_unix_sockets: nonempty_vec(sandbox_string_array(
                Some(settings),
                "/network/allowUnixSockets",
            )),
            allow_all_unix_sockets: sandbox_bool(Some(settings), "/network/allowAllUnixSockets"),
            allow_local_binding: sandbox_bool(Some(settings), "/network/allowLocalBinding"),
            http_proxy_port: sandbox_u16(Some(settings), "/network/httpProxyPort"),
            socks_proxy_port: sandbox_u16(Some(settings), "/network/socksProxyPort"),
        },
        filesystem: SandboxFilesystemConfig {
            deny_read,
            allow_read,
            allow_write,
            deny_write,
        },
        ignore_violations: sandbox_value(Some(settings), "/ignoreViolations"),
        enable_weaker_nested_sandbox: sandbox_bool(Some(settings), "/enableWeakerNestedSandbox"),
        enable_weaker_network_isolation: sandbox_bool(
            Some(settings),
            "/enableWeakerNetworkIsolation",
        ),
        ripgrep: sandbox_value(Some(settings), "/ripgrep"),
    }
}

fn nonempty_vec(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// Maps to: CC `getSandboxEnabledSetting()`.
pub fn get_sandbox_enabled_setting(settings: &SettingsJson) -> bool {
    sandbox_bool(Some(settings), "/enabled").unwrap_or(false)
}

/// Maps to: CC `isAutoAllowBashIfSandboxedEnabled()`.
pub fn is_auto_allow_bash_if_sandboxed_enabled(settings: &SettingsJson) -> bool {
    sandbox_bool(Some(settings), "/autoAllowBashIfSandboxed").unwrap_or(true)
}

/// Maps to: CC `areUnsandboxedCommandsAllowed()`.
pub fn are_unsandboxed_commands_allowed(settings: &SettingsJson) -> bool {
    sandbox_bool(Some(settings), "/allowUnsandboxedCommands").unwrap_or(true)
}

/// Maps to: CC `isSandboxRequired()`.
pub fn is_sandbox_required(settings: &SettingsJson) -> bool {
    get_sandbox_enabled_setting(settings)
        && sandbox_bool(Some(settings), "/failIfUnavailable").unwrap_or(false)
}

/// Maps to: CC `getExcludedCommands()`.
pub fn get_excluded_commands(settings: &SettingsJson) -> Vec<String> {
    sandbox_string_array(Some(settings), "/excludedCommands")
}

/// Maps to: CC `SandboxManager.setSandboxSettings(...)` options object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SandboxSettingsUpdate {
    pub enabled: Option<bool>,
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    pub allow_unsandboxed_commands: Option<bool>,
}

fn local_settings_path() -> anyhow::Result<PathBuf> {
    get_settings_file_path_for_source(SettingSource::Local)
        .ok_or_else(|| anyhow::anyhow!("local settings path is unavailable"))
}

fn read_local_settings_value() -> anyhow::Result<serde_json::Value> {
    let path = local_settings_path()?;
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str::<serde_json::Value>(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(error.into()),
    }
}

fn write_local_settings_value(value: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write as _;
    if !crate::utils::session_storage::is_session_write_enabled() {
        anyhow::bail!(crate::tools::shared::write_gate::SANDBOX_SETTINGS_DISABLED_ERROR);
    }
    let path = local_settings_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("sandbox settings path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".sandbox-settings-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        Ok::<_, anyhow::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// Maps to: CC `sandbox-adapter.ts:669-691#setSandboxSettings`. Its promise
/// never rejects: it calls `updateSettingsForSource('localSettings', …)` and
/// drops the `{ error }` that function returns after logging it
/// (settings.ts:515-520). A failed write is logged here the same way and
/// the caller carries on.
pub fn set_sandbox_settings(options: SandboxSettingsUpdate) {
    if let Err(error) = write_sandbox_settings_update(options) {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
    }
}

fn write_sandbox_settings_update(options: SandboxSettingsUpdate) -> anyhow::Result<()> {
    let mut value = read_local_settings_value()?;
    if !value.is_object() {
        value = serde_json::json!({});
    }
    let root = value.as_object_mut().expect("object checked above");
    let sandbox = root
        .entry("sandbox".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !sandbox.is_object() {
        *sandbox = serde_json::json!({});
    }
    let sandbox_object = sandbox.as_object_mut().expect("object checked above");
    if let Some(enabled) = options.enabled {
        sandbox_object.insert("enabled".to_string(), serde_json::Value::Bool(enabled));
    }
    if let Some(auto_allow) = options.auto_allow_bash_if_sandboxed {
        sandbox_object.insert(
            "autoAllowBashIfSandboxed".to_string(),
            serde_json::Value::Bool(auto_allow),
        );
    }
    if let Some(allow_unsandboxed) = options.allow_unsandboxed_commands {
        sandbox_object.insert(
            "allowUnsandboxedCommands".to_string(),
            serde_json::Value::Bool(allow_unsandboxed),
        );
    }
    write_local_settings_value(&value)
}

fn sandbox_excluded_command_pattern(
    command: &str,
    permission_updates: Option<&[PermissionUpdate]>,
) -> String {
    let mut command_pattern = command.to_string();

    if let Some(updates) = permission_updates {
        for update in updates {
            let PermissionUpdate::AddRules { rules, .. } = update else {
                continue;
            };
            if !rules.iter().any(|rule| rule.tool_name == BASH_TOOL_NAME) {
                continue;
            }
            if let Some(rule) = rules.iter().find(|rule| rule.tool_name == BASH_TOOL_NAME) {
                if let Some(rule_content) = rule.rule_content.as_deref() {
                    command_pattern = permission_rule_extract_prefix(rule_content)
                        .unwrap_or_else(|| rule_content.to_string());
                }
            }
            break;
        }
    }

    command_pattern
}

/// Maps to: CC `addToExcludedCommands(...)`.
pub fn add_to_excluded_commands(
    command: &str,
    permission_updates: Option<&[PermissionUpdate]>,
) -> anyhow::Result<String> {
    let command_pattern = sandbox_excluded_command_pattern(command, permission_updates);
    let mut value = read_local_settings_value()?;
    if !value.is_object() {
        value = serde_json::json!({});
    }
    let root = value.as_object_mut().expect("object checked above");
    let sandbox = root
        .entry("sandbox".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !sandbox.is_object() {
        *sandbox = serde_json::json!({});
    }
    let sandbox_object = sandbox.as_object_mut().expect("object checked above");
    let existing = sandbox_object
        .get("excludedCommands")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !existing.iter().any(|value| value == &command_pattern) {
        let mut updated = existing
            .into_iter()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>();
        updated.push(serde_json::Value::String(command_pattern.clone()));
        sandbox_object.insert(
            "excludedCommands".to_string(),
            serde_json::Value::Array(updated),
        );
        write_local_settings_value(&value)?;
    }

    Ok(command_pattern)
}

/// Maps to: CC `areSandboxSettingsLockedByPolicy()`.
pub fn are_sandbox_settings_locked_by_policy() -> bool {
    [SettingSource::Flag, SettingSource::Policy]
        .into_iter()
        .filter_map(get_settings_for_source)
        .any(|settings| {
            settings.sandbox.as_ref().is_some_and(|sandbox| {
                sandbox.get("enabled").is_some()
                    || sandbox.get("autoAllowBashIfSandboxed").is_some()
                    || sandbox.get("allowUnsandboxedCommands").is_some()
            })
        })
}

// --- `@anthropic-ai/sandbox-runtime` SandboxViolationStore (in-process) ----

/// Maps to: `@anthropic-ai/sandbox-runtime` `SandboxViolationEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxViolationEvent {
    pub line: String,
    pub command: Option<String>,
    pub encoded_command: Option<String>,
    pub timestamp: SystemTime,
}

/// Listener receives `(total_count, delta)` after a record batch.
///
/// Official runtime `subscribe` passes the violations array; footer hint
/// (CC `SandboxPromptFooterHint`) only needs the delta vs last total. Cometix
/// notifies `(total, delta)` so the PromptInput subscriber stays allocation-light.
pub type SandboxViolationListener = Arc<dyn Fn(u32, u32) + Send + Sync>;

/// Opaque subscribe handle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SandboxViolationListenerId(u64);

struct SandboxViolationStoreInner {
    violations: Vec<SandboxViolationEvent>,
    total_count: u32,
    max_size: usize,
    next_listener_id: u64,
    listeners: HashMap<u64, SandboxViolationListener>,
}

/// In-process stand-in for `@anthropic-ai/sandbox-runtime` `SandboxViolationStore`.
///
/// Maps to CC `export { SandboxViolationStore }` from `sandbox-adapter.ts`.
pub struct SandboxViolationStore {
    inner: Mutex<SandboxViolationStoreInner>,
}

impl Default for SandboxViolationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxViolationStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SandboxViolationStoreInner {
                violations: Vec::new(),
                total_count: 0,
                max_size: 100,
                next_listener_id: 1,
                listeners: HashMap::new(),
            }),
        }
    }

    pub fn get_total_count(&self) -> u32 {
        let _turn = crate::state::store::enter_store_turn_segment();
        self.inner
            .lock()
            .map(|inner| inner.total_count)
            .unwrap_or(0)
    }

    pub fn get_count(&self) -> usize {
        let _turn = crate::state::store::enter_store_turn_segment();
        self.inner
            .lock()
            .map(|inner| inner.violations.len())
            .unwrap_or(0)
    }

    pub fn get_violations(&self, limit: Option<usize>) -> Vec<SandboxViolationEvent> {
        let _turn = crate::state::store::enter_store_turn_segment();
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        match limit {
            Some(n) => {
                let start = inner.violations.len().saturating_sub(n);
                inner.violations[start..].to_vec()
            }
            None => inner.violations.clone(),
        }
    }

    /// Maps to runtime `addViolation` (single event).
    pub fn add_violation(&self, event: SandboxViolationEvent) {
        self.record_events(std::iter::once(event));
    }

    /// Record `count` anonymous violation lines (footer / batch helper).
    pub fn record(&self, count: u32) {
        if count == 0 {
            return;
        }
        let now = SystemTime::now();
        self.record_events((0..count).map(|_| SandboxViolationEvent {
            line: String::new(),
            command: None,
            encoded_command: None,
            timestamp: now,
        }));
    }

    fn record_events<I>(&self, events: I)
    where
        I: IntoIterator<Item = SandboxViolationEvent>,
    {
        // Contract A clause 6: each synchronous public operation of this
        // non-`store.ts` turn member (mutation + its notify pass) is one turn
        // segment; the inner lock is released before every callback, and a
        // listener re-entering AppStore.set_state nests on the same thread.
        // Registry/notification semantics stay owner-defined (clauses 1/3/5
        // do not extend here).
        let _turn = crate::state::store::enter_store_turn_segment();
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let mut delta = 0u32;
        for event in events {
            inner.violations.push(event);
            inner.total_count = inner.total_count.saturating_add(1);
            delta = delta.saturating_add(1);
        }
        if delta == 0 {
            return;
        }
        if inner.violations.len() > inner.max_size {
            let overflow = inner.violations.len() - inner.max_size;
            inner.violations.drain(0..overflow);
        }
        let total = inner.total_count;
        let listeners: Vec<_> = inner.listeners.values().cloned().collect();
        drop(inner);
        for listener in listeners {
            listener(total, delta);
        }
    }

    pub fn clear(&self) {
        let _turn = crate::state::store::enter_store_turn_segment();
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.violations.clear();
        // Official clear() does not reset totalCount; keep the same semantics.
        let total = inner.total_count;
        let listeners: Vec<_> = inner.listeners.values().cloned().collect();
        drop(inner);
        for listener in listeners {
            listener(total, 0);
        }
    }

    pub fn subscribe<F>(&self, listener: F) -> SandboxViolationListenerId
    where
        F: Fn(u32, u32) + Send + Sync + 'static,
    {
        // Turn segment covers registration AND the immediate seed callback
        // (Contract A clause 6: "subscribe 的立即回调" is part of the segment).
        let _turn = crate::state::store::enter_store_turn_segment();
        let Ok(mut inner) = self.inner.lock() else {
            return SandboxViolationListenerId(0);
        };
        let id = inner.next_listener_id;
        inner.next_listener_id = inner.next_listener_id.wrapping_add(1).max(1);
        let listener = Arc::new(listener) as SandboxViolationListener;
        // Official subscribe immediately invokes with current violations; footer
        // only reacts to positive deltas, so seed with (total, 0).
        let total = inner.total_count;
        inner.listeners.insert(id, Arc::clone(&listener));
        drop(inner);
        listener(total, 0);
        SandboxViolationListenerId(id)
    }

    pub fn unsubscribe(&self, id: SandboxViolationListenerId) {
        let _turn = crate::state::store::enter_store_turn_segment();
        if let Ok(mut inner) = self.inner.lock() {
            inner.listeners.remove(&id.0);
        }
    }

    #[cfg(test)]
    pub fn reset_for_test(&self) {
        let _turn = crate::state::store::enter_store_turn_segment();
        if let Ok(mut inner) = self.inner.lock() {
            inner.violations.clear();
            inner.total_count = 0;
            inner.listeners.clear();
            inner.next_listener_id = 1;
        }
    }
}

/// Maps to: CC `SandboxManager.getSandboxViolationStore()`.
pub fn get_sandbox_violation_store() -> &'static SandboxViolationStore {
    static STORE: OnceLock<SandboxViolationStore> = OnceLock::new();
    STORE.get_or_init(SandboxViolationStore::new)
}

/// Record `count` violations on the shared store.
pub fn record_sandbox_violations(count: u32) {
    get_sandbox_violation_store().record(count);
}

/// Maps to `SandboxManager.annotateStderrWithSandboxFailures`. The external
/// runtime normally obtains structured denial events from Seatbelt/bwrap; this
/// adapter recognizes their stable failure text and emits the same model-visible
/// violation block with an explicit unsandboxed-retry hint.
pub fn annotate_stderr_with_sandbox_failures(_command: &str, output: &str) -> String {
    if output.contains("<sandbox_violations>") {
        return output.to_string();
    }
    let violation = output.lines().find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("operation not permitted")
            || lower.contains("permission denied")
            || lower.contains("sandbox-exec:")
            || lower.contains("bwrap:")
    });
    let Some(violation) = violation else {
        return output.to_string();
    };
    let mut violation = violation.trim().replace('<', "[").replace('>', "]");
    if violation.chars().count() > 500 {
        violation = violation.chars().take(500).collect();
    }
    let separator = if output.is_empty() || output.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!(
        "{output}{separator}<sandbox_violations>\nSandbox blocked this operation: {violation}. Retry with dangerouslyDisableSandbox=true only if the operation is necessary.\n</sandbox_violations>"
    )
}

/// Parse `<sandbox_violations>` blocks from stderr and record each non-empty line.
pub fn record_sandbox_violations_from_stderr(stderr: &str) {
    let lines = parse_sandbox_violation_lines(stderr);
    if lines.is_empty() {
        return;
    }
    let now = SystemTime::now();
    get_sandbox_violation_store().record_events(lines.into_iter().map(|line| {
        SandboxViolationEvent {
            line,
            command: None,
            encoded_command: None,
            timestamp: now,
        }
    }));
}

fn parse_sandbox_violation_lines(stderr: &str) -> Vec<String> {
    const START: &str = "<sandbox_violations>";
    const END: &str = "</sandbox_violations>";
    let mut lines = Vec::new();
    let mut rest = stderr;
    while let Some(start) = rest.find(START) {
        let after_start = &rest[start + START.len()..];
        if let Some(end) = after_start.find(END) {
            let body = &after_start[..end];
            for line in body.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_string());
                }
            }
            rest = &after_start[end + END.len()..];
        } else {
            break;
        }
    }
    lines
}

fn expand_path(pattern: &str, root: &Path) -> String {
    let trimmed = pattern.trim();
    if trimmed == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return normalize_path_string(PathBuf::from(home).join(rest));
        }
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        normalize_path_string(path.to_path_buf())
    } else {
        normalize_path_string(root.join(path))
    }
}

fn normalize_path_string(path: PathBuf) -> String {
    path.components().collect::<PathBuf>().display().to_string()
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::env_utils::TEST_ENV_LOCK;
    use std::path::Path;

    struct EnvGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, path: &Path) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::set(key, path),
            }
        }
    }

    struct CwdGuard {
        old: PathBuf,
        old_original_cwd: PathBuf,
    }

    impl CwdGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::current_dir().unwrap();
            let old_original_cwd = crate::bootstrap::state::get_original_cwd();
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            crate::bootstrap::state::set_original_cwd(&path);
            std::env::set_current_dir(path).unwrap();
            Self {
                old,
                old_original_cwd,
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.old);
            crate::bootstrap::state::set_original_cwd(&self.old_original_cwd);
        }
    }

    fn constant_ask_callback(allow: bool) -> SandboxAskCallback {
        Arc::new(move |_pattern: NetworkHostPattern| {
            Box::pin(async move { allow })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        })
    }

    #[test]
    fn cleared_ask_callback_denies_network_host_permission() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        clear_sandbox_ask_callback_for_test();
        assert!(!futures::executor::block_on(
            ask_for_network_host_permission(NetworkHostPattern::new("api.example.com"))
        ));
    }

    #[test]
    fn ask_callback_registration_guard_clears_only_its_own_slot_entry() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        clear_sandbox_ask_callback_for_test();

        // Simulate an installed registration without the settings gate
        // (`register` itself is gated on isSandboxingEnabled()).
        let first = constant_ask_callback(true);
        *sandbox_ask_callback_slot().lock().unwrap() = Some(Arc::clone(&first));
        let first_guard = SandboxAskCallbackRegistration {
            registered: Some(Arc::clone(&first)),
        };

        // A successor REPL re-registers before the old guard drops: the stale
        // guard must not clear the successor's registration.
        let second = constant_ask_callback(false);
        *sandbox_ask_callback_slot().lock().unwrap() = Some(Arc::clone(&second));
        drop(first_guard);
        assert!(
            sandbox_ask_callback_slot().lock().unwrap().is_some(),
            "stale guard must not clear a successor registration"
        );

        // The live guard clears its own registration on drop → subsequent
        // asks deny (slot empty).
        let second_guard = SandboxAskCallbackRegistration {
            registered: Some(second),
        };
        drop(second_guard);
        assert!(sandbox_ask_callback_slot().lock().unwrap().is_none());
        assert!(!futures::executor::block_on(
            ask_for_network_host_permission(NetworkHostPattern::new("api.example.com"))
        ));
        clear_sandbox_ask_callback_for_test();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_wrapper_enforces_write_boundary() {
        let root = std::env::temp_dir().join(format!(
            "cometix-seatbelt-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let outside = root.with_extension("outside");
        let inside = root.join("inside.txt");
        let command = format!(
            "printf inside > {}; printf outside > {}",
            crate::utils::bash::shell_quote::quote(&[&inside.display().to_string()]),
            crate::utils::bash::shell_quote::quote(&[&outside.display().to_string()]),
        );
        let wrapped = wrap_shell_command_macos(
            "bash",
            &command,
            &root,
            &SandboxRuntimeConfig::default(),
            &crate::utils::sandbox::network_proxy::NetworkProxyEndpoints {
                http_port: 1,
                socks_port: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let output = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&root)
            .output()
            .unwrap();

        assert!(inside.is_file(), "sandbox must allow writes under cwd");
        assert!(!outside.exists(), "sandbox must deny writes outside cwd");
        assert!(!output.status.success());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_routes_allowed_http_through_domain_filtering_proxy() {
        use std::io::{Read as _, Write as _};
        use std::net::{Ipv4Addr, TcpListener};

        struct ProxyGuard;
        impl Drop for ProxyGuard {
            fn drop(&mut self) {
                crate::utils::sandbox::network_proxy::reset_network_proxy_for_test();
                clear_sandbox_ask_callback_for_test();
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = ProxyGuard;
        crate::utils::sandbox::network_proxy::reset_network_proxy_for_test();
        clear_sandbox_ask_callback_for_test();
        let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = origin.local_addr().unwrap().port();
        let origin_thread = std::thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nproxied",
                )
                .unwrap();
        });
        let root = std::env::temp_dir().join(format!(
            "cometix-seatbelt-proxy-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut settings = SettingsJson::default();
        settings.sandbox = Some(serde_json::json!({
            "enabled": true,
            "network": {"allowedDomains": ["127.0.0.1"]}
        }));
        let command =
            format!("/usr/bin/curl --silent --show-error --noproxy '' http://127.0.0.1:{port}/");
        let wrapped = wrap_shell_command("/bin/bash", &command, &root, &settings).unwrap();
        let output = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&root)
            .output()
            .unwrap();
        origin_thread.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "proxied");
        cleanup_after_command(&wrapped.cleanup_paths);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn write_allowlist_rejects_symlink_escape_paths() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-write-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        assert!(sandbox_allow_write_paths(&["escape".to_string()], &root).is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_wrapper_blocks_mandatory_shell_startup_files() {
        let root = std::env::temp_dir().join(format!(
            "cometix-seatbelt-mandatory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let protected = root.join(".bashrc");
        let ordinary = root.join("ordinary");
        std::fs::write(&protected, "original").unwrap();
        let command = format!(
            "printf ordinary > {}; printf attacked > {}",
            crate::utils::bash::shell_quote::quote(&[&ordinary.display().to_string()]),
            crate::utils::bash::shell_quote::quote(&[&protected.display().to_string()]),
        );
        let wrapped = wrap_shell_command_macos(
            "bash",
            &command,
            &root,
            &SandboxRuntimeConfig::default(),
            &crate::utils::sandbox::network_proxy::NetworkProxyEndpoints {
                http_port: 1,
                socks_port: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let output = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&protected).unwrap(), "original");
        assert!(
            !output.status.success(),
            "stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(&ordinary).unwrap(), "ordinary");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn managed_sandbox_domains_only_reads_policy_settings_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-managed-sandbox-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("managed-settings.json"),
            r#"{"sandbox":{"network":{"allowManagedDomainsOnly":true}}}"#,
        )
        .unwrap();
        let _managed_guard = EnvGuard::set_path("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);

        assert!(should_allow_managed_sandbox_domains_only());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn managed_sandbox_domains_only_defaults_false_without_policy() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-empty-managed-sandbox-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _managed_guard = EnvGuard::set_path("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);

        assert!(!should_allow_managed_sandbox_domains_only());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_path_resolvers_match_official_permission_vs_filesystem_semantics() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-paths-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let _cwd_guard = CwdGuard::set(&root);
        let cwd = std::env::current_dir().unwrap();

        assert_eq!(
            resolve_path_pattern_for_sandbox("//.aws/**", SettingSource::Project),
            "/.aws/**"
        );
        assert_eq!(
            resolve_path_pattern_for_sandbox("/src/**", SettingSource::Project),
            cwd.join("src/**").display().to_string()
        );
        assert_eq!(
            resolve_path_pattern_for_sandbox("~/cache", SettingSource::Project),
            "~/cache"
        );
        assert_eq!(
            resolve_sandbox_filesystem_path("//Users/test/.cargo", SettingSource::Project),
            "/Users/test/.cargo"
        );
        assert_eq!(
            resolve_sandbox_filesystem_path("/Users/test/.cargo", SettingSource::Project),
            "/Users/test/.cargo"
        );
        assert_eq!(
            resolve_sandbox_filesystem_path("relative", SettingSource::Project),
            cwd.join("relative").display().to_string()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_runtime_config_extracts_network_and_filesystem_settings_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-config-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let managed = root.join("managed");
        let config_home = root.join("user-config");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::write(
            root.join(".claude/settings.json"),
            r#"{
              "sandbox": {
                "network": {
                  "allowedDomains": ["project.example"],
                  "allowUnixSockets": ["/tmp/project.sock"],
                  "allowLocalBinding": true,
                  "httpProxyPort": 1271,
                  "socksProxyPort": 1272
                },
                "filesystem": {
                  "allowWrite": ["sandbox-write"],
                  "denyWrite": ["/absolute-deny"],
                  "denyRead": ["//etc/shadow"],
                  "allowRead": ["sandbox-read"]
                },
                "ignoreViolations": {"read": ["*.tmp"]},
                "enableWeakerNestedSandbox": true,
                "excludedCommands": ["npm run test:*"]
              },
              "permissions": {
                "allow": ["WebFetch(domain:allowed.example)", "Edit(/src/**)"],
                "deny": ["WebFetch(domain:denied.example)", "Read(//etc/passwd)", "Edit(~/blocked)"],
                "additionalDirectories": ["/extra-work"]
              }
            }"#,
        )
        .unwrap();
        let _cwd_guard = CwdGuard::set(&root);
        let _managed_guard = EnvGuard::set_path("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _config_guard = EnvGuard::set_path("CLAUDE_CONFIG_DIR", &config_home);

        let settings = get_initial_settings();
        let config = convert_to_sandbox_runtime_config(&settings);
        let cwd = std::env::current_dir().unwrap();

        assert_eq!(
            config.network.allowed_domains,
            vec!["project.example".to_string(), "allowed.example".to_string()]
        );
        assert_eq!(config.network.denied_domains, vec!["denied.example"]);
        assert_eq!(
            config.network.allow_unix_sockets,
            Some(vec!["/tmp/project.sock".to_string()])
        );
        assert_eq!(config.network.allow_local_binding, Some(true));
        assert_eq!(config.network.http_proxy_port, Some(1271));
        assert_eq!(config.network.socks_proxy_port, Some(1272));
        assert!(
            config
                .filesystem
                .allow_write
                .contains(&cwd.join("src/**").display().to_string())
        );
        assert!(
            config
                .filesystem
                .allow_write
                .contains(&cwd.join("sandbox-write").display().to_string())
        );
        assert!(
            config
                .filesystem
                .allow_write
                .contains(&"/extra-work".to_string())
        );
        assert!(
            config
                .filesystem
                .deny_write
                .contains(&"/absolute-deny".to_string())
        );
        assert!(
            config
                .filesystem
                .deny_write
                .contains(&"~/blocked".to_string())
        );
        assert!(
            config
                .filesystem
                .deny_read
                .contains(&"/etc/passwd".to_string())
        );
        assert!(
            config
                .filesystem
                .deny_read
                .contains(&"/etc/shadow".to_string())
        );
        assert!(
            config
                .filesystem
                .allow_read
                .contains(&cwd.join("sandbox-read").display().to_string())
        );
        assert_eq!(config.enable_weaker_nested_sandbox, Some(true));
        assert_eq!(get_excluded_commands(&settings), vec!["npm run test:*"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_runtime_config_honors_managed_only_network_and_read_policies() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-managed-config-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let managed = root.join("managed");
        let config_home = root.join("user-config");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::write(
            root.join(".claude/settings.json"),
            r#"{
              "sandbox": {
                "network": {"allowedDomains": ["project.example"]},
                "filesystem": {"allowRead": ["project-read"]}
              },
              "permissions": {
                "allow": ["WebFetch(domain:project-allow.example)"],
                "deny": ["WebFetch(domain:project-deny.example)"]
              }
            }"#,
        )
        .unwrap();
        std::fs::write(
            managed.join("managed-settings.json"),
            r#"{
              "sandbox": {
                "enabled": true,
                "network": {
                  "allowManagedDomainsOnly": true,
                  "allowedDomains": ["managed.example"]
                },
                "filesystem": {
                  "allowManagedReadPathsOnly": true,
                  "allowRead": ["managed-read"]
                }
              },
              "permissions": {
                "allow": ["WebFetch(domain:policy-allow.example)"]
              }
            }"#,
        )
        .unwrap();
        let _cwd_guard = CwdGuard::set(&root);
        let _managed_guard = EnvGuard::set_path("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _config_guard = EnvGuard::set_path("CLAUDE_CONFIG_DIR", &config_home);

        let settings = get_initial_settings();
        let config = convert_to_sandbox_runtime_config(&settings);
        let cwd = std::env::current_dir().unwrap();

        assert_eq!(
            config.network.allowed_domains,
            vec![
                "managed.example".to_string(),
                "policy-allow.example".to_string()
            ]
        );
        assert_eq!(
            config.network.denied_domains,
            vec!["project-deny.example".to_string()]
        );
        assert!(
            config
                .filesystem
                .allow_read
                .contains(&cwd.join("managed-read").display().to_string())
        );
        assert!(
            !config
                .filesystem
                .allow_read
                .contains(&cwd.join("project-read").display().to_string()),
            "managed allow-read policy should suppress non-policy allowRead paths"
        );
        assert!(are_sandbox_settings_locked_by_policy());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandboxing_enabled_uses_canonical_process_settings_and_runtime_gates() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-enabled-runtime-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_home = root.join("config");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&config_home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let current_platform = if crate::utils::env::is_wsl() {
            "wsl"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            std::env::consts::OS
        };
        std::fs::write(
            config_home.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "sandbox": {
                    "enabled": true,
                    "enabledPlatforms": [current_platform]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        {
            let _cwd_guard = CwdGuard::set(&workspace);
            let _config_guard = EnvGuard::set_path("CLAUDE_CONFIG_DIR", &config_home);
            let _managed_guard = EnvGuard::set_path(
                "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
                &root.join("missing-managed-settings.json"),
            );

            let expected =
                is_supported_platform() && check_dependencies_readonly().errors.is_empty();
            assert_eq!(is_sandboxing_enabled(), expected);

            std::fs::write(
                config_home.join("settings.json"),
                br#"{"sandbox":{"enabled":true,"enabledPlatforms":[]}}"#,
            )
            .unwrap();
            // Direct writes bypass `update_settings_for_source`, the only
            // writer that invalidates the session-wide merged-settings cache
            // (settings.ts:505-506); the assertion above already populated it.
            crate::utils::settings::settings_cache::reset_settings_cache();
            assert!(!is_sandboxing_enabled());

            std::fs::write(
                config_home.join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "sandbox": {
                        "enabled": false,
                        "enabledPlatforms": [current_platform]
                    }
                }))
                .unwrap(),
            )
            .unwrap();
            crate::utils::settings::settings_cache::reset_settings_cache();
            assert!(!is_sandboxing_enabled());
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sandbox_setting_gates_match_official_defaults() {
        let default_settings = SettingsJson::default();
        assert!(!get_sandbox_enabled_setting(&default_settings));
        assert!(is_auto_allow_bash_if_sandboxed_enabled(&default_settings));
        assert!(are_unsandboxed_commands_allowed(&default_settings));
        assert!(!is_sandbox_required(&default_settings));

        let settings: SettingsJson = serde_json::from_value(serde_json::json!({
            "sandbox": {
                "enabled": true,
                "autoAllowBashIfSandboxed": false,
                "allowUnsandboxedCommands": false,
                "failIfUnavailable": true
            }
        }))
        .unwrap();
        assert!(get_sandbox_enabled_setting(&settings));
        assert!(!is_auto_allow_bash_if_sandboxed_enabled(&settings));
        assert!(!are_unsandboxed_commands_allowed(&settings));
        assert!(is_sandbox_required(&settings));
    }

    #[test]
    fn bare_git_repo_markers_are_denied_or_scrubbed_without_host_stubs() {
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-bare-git-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("HEAD"), "legitimate-existing").unwrap();
        let mut config = SandboxRuntimeConfig::default();
        let cleanup = protect_against_bare_git_repo_planting(&root, &mut config);
        assert!(
            config
                .filesystem
                .deny_write
                .contains(&root.join("HEAD").display().to_string())
        );
        assert!(cleanup.contains(&root.join("refs")));
        assert!(
            !root.join("refs").exists(),
            "protection must not plant a stub"
        );

        std::fs::create_dir_all(root.join("refs")).unwrap();
        std::fs::write(root.join("config"), "[core]").unwrap();
        let local_cleanup = cleanup
            .into_iter()
            .filter(|path| path.starts_with(&root))
            .collect::<Vec<_>>();
        cleanup_after_command(&local_cleanup);
        assert!(!root.join("refs").exists());
        assert!(!root.join("config").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("HEAD")).unwrap(),
            "legitimate-existing"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worktree_gitdir_resolves_main_repo_write_exception() {
        let root = std::env::temp_dir().join(format!(
            "cometix-sandbox-worktree-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join("main/.git/worktrees/feature").display()
            ),
        )
        .unwrap();
        assert_eq!(
            detect_worktree_main_repo(&worktree),
            Some(root.join("main"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn permission_rule_extract_prefix_matches_official_suffix_rule() {
        assert_eq!(
            permission_rule_extract_prefix("npm run test:*"),
            Some("npm run test".to_string())
        );
        assert_eq!(permission_rule_extract_prefix("npm run test"), None);
        assert_eq!(permission_rule_extract_prefix(":*"), None);
    }

    #[test]
    fn violation_store_records_notifies_and_parses_stderr() {
        struct ResetStore;
        impl Drop for ResetStore {
            fn drop(&mut self) {
                get_sandbox_violation_store().reset_for_test();
            }
        }

        let _lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _reset = ResetStore;
        let store = get_sandbox_violation_store();
        store.reset_for_test();

        let seen = std::sync::Mutex::new(Vec::<(u32, u32)>::new());
        let seen_for_listener = std::sync::Arc::new(seen);
        let seen_clone = std::sync::Arc::clone(&seen_for_listener);
        let id = store.subscribe(move |total, delta| {
            seen_clone.lock().unwrap().push((total, delta));
        });
        // subscribe seeds (total, 0)
        assert_eq!(seen_for_listener.lock().unwrap().as_slice(), &[(0, 0)]);

        record_sandbox_violations(2);
        assert_eq!(store.get_total_count(), 2);
        assert_eq!(
            seen_for_listener.lock().unwrap().as_slice(),
            &[(0, 0), (2, 2)]
        );

        record_sandbox_violations_from_stderr(
            "noise\n<sandbox_violations>\nline-a\nline-b\n</sandbox_violations>\ntrail",
        );
        assert_eq!(store.get_total_count(), 4);
        let events = store.get_violations(Some(2));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].line, "line-a");
        assert_eq!(events[1].line, "line-b");

        let annotated = annotate_stderr_with_sandbox_failures(
            "touch /blocked",
            "touch: /blocked: Operation not permitted",
        );
        assert!(annotated.contains("<sandbox_violations>"));
        assert!(annotated.contains("dangerouslyDisableSandbox=true"));
        record_sandbox_violations_from_stderr(&annotated);
        assert_eq!(store.get_total_count(), 5);

        store.unsubscribe(id);
        store.reset_for_test();
        assert_eq!(store.get_total_count(), 0);
    }

    /// Contract A clause 6 evidence: a violation listener re-entering
    /// `AppStore::set_state` nests on the same thread inside the
    /// `record_events` turn segment (the cross-store direction that motivated
    /// the process-wide turn). The clause-6 FileRead ordering debug_assert is
    /// exercised implicitly by every FileRead test running under
    /// debug_assertions; a #[should_panic] probe is not written because the
    /// assert fires inside production `FileReadSourceTurn::run` and poisoning
    /// its process-wide mutex would cascade into unrelated tests.
    #[test]
    fn violation_listener_reenters_app_store_turn_on_same_thread() {
        let _lock = crate::utils::env_utils::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = get_sandbox_violation_store();
        store.reset_for_test();

        let app_store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let app_store_in_listener = app_store.clone();
        let id = store.subscribe(move |total, delta| {
            if delta == 0 {
                return;
            }
            // Same-thread nested turn entry: segment (record_events) →
            // AppStore::replace_with. A non-re-entrant turn would deadlock
            // here.
            app_store_in_listener
                .replace_with(|state| state.status_line_text = Some(format!("{total}")));
        });

        record_sandbox_violations(3);
        assert_eq!(app_store.get().status_line_text.as_deref(), Some("3"));

        store.unsubscribe(id);
        store.reset_for_test();
    }
}
