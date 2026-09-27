//! ```ts
//! export const env = {
//!   platform,         // 'win32' | 'darwin' | 'linux'
//!   arch,             // process.arch
//!   terminal,         // detectTerminal()
//!   isCI,             // isEnvTruthy('CI')
//!   isSSH,            // isSSHSession()
//!   ...
//! }
//! ```

use std::sync::OnceLock;

static ENV: OnceLock<Env> = OnceLock::new();

pub fn get() -> &'static Env {
    ENV.get_or_init(Env::detect)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
}

/// Maps to CC `getPlatform() === 'wsl'` detection used by timeout and path
/// security policy.
pub fn is_wsl() -> bool {
    if crate::utils::process_env::var_os("WSL_DISTRO_NAME").is_some()
        || crate::utils::process_env::var_os("WSL_INTEROP").is_some()
    {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        return match crate::utils::fs_operations::get_fs_implementation().read_file_sync(
            std::path::Path::new("/proc/version"),
            crate::utils::fs_operations::BufferEncoding::Utf8,
        ) {
            Ok(version) => {
                let version = version.to_string_lossy().to_lowercase();
                version.contains("microsoft") || version.contains("wsl")
            }
            Err(error) => {
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                false
            }
        };
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[derive(Debug)]
pub struct Env {
    pub platform: Platform,
    pub terminal: Option<String>,
    pub is_ci: bool,
    pub is_ssh: bool,
}

impl Env {
    fn detect() -> Self {
        Self {
            platform: detect_platform(),
            terminal: detect_terminal(),
            is_ci: crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CI").ok().as_deref(),
            ),
            is_ssh: is_ssh_session(),
        }
    }
}

fn detect_terminal() -> Option<String> {
    if crate::utils::process_env::var("CURSOR_TRACE_ID").is_ok() {
        return Some("cursor".into());
    }
    if let Ok(askpass) = crate::utils::process_env::var("VSCODE_GIT_ASKPASS_MAIN") {
        if askpass.contains("cursor") {
            return Some("cursor".into());
        }
        if askpass.contains("windsurf") {
            return Some("windsurf".into());
        }
        if askpass.contains("antigravity") {
            return Some("antigravity".into());
        }
    }
    // __CFBundleIdentifier (macOS, lines 147-156)
    if let Ok(bundle) = crate::utils::process_env::var("__CFBundleIdentifier") {
        let lower = bundle.to_lowercase();
        if lower.contains("vscodium") {
            return Some("codium".into());
        }
        if lower.contains("windsurf") {
            return Some("windsurf".into());
        }
    }

    // Visual Studio (line 158)
    if crate::utils::process_env::var("VisualStudioVersion").is_ok() {
        return Some("visualstudio".into());
    }

    // JetBrains (lines 164-169)
    if crate::utils::process_env::var("TERMINAL_EMULATOR").as_deref() == Ok("JetBrains-JediTerm") {
        return Some("pycharm".into());
    }

    if let Ok(term) = crate::utils::process_env::var("TERM") {
        if term == "xterm-ghostty" {
            return Some("ghostty".into());
        }
        if term.contains("kitty") {
            return Some("kitty".into());
        }
    }

    // Apple_Terminal, iTerm.app, WezTerm, vscode, WarpTerminal, etc.
    if let Ok(tp) = crate::utils::process_env::var("TERM_PROGRAM") {
        return Some(tp);
    }

    // tmux / screen (lines 185-186)
    if crate::utils::process_env::var("TMUX").is_ok() {
        return Some("tmux".into());
    }
    if crate::utils::process_env::var("STY").is_ok() {
        return Some("screen".into());
    }

    if crate::utils::process_env::var("KONSOLE_VERSION").is_ok() {
        return Some("konsole".into());
    }
    if crate::utils::process_env::var("GNOME_TERMINAL_SERVICE").is_ok() {
        return Some("gnome-terminal".into());
    }
    if crate::utils::process_env::var("XTERM_VERSION").is_ok() {
        return Some("xterm".into());
    }
    if crate::utils::process_env::var("VTE_VERSION").is_ok() {
        return Some("vte-based".into());
    }
    if crate::utils::process_env::var("TERMINATOR_UUID").is_ok() {
        return Some("terminator".into());
    }
    if crate::utils::process_env::var("KITTY_WINDOW_ID").is_ok() {
        return Some("kitty".into());
    }
    if crate::utils::process_env::var("ALACRITTY_LOG").is_ok() {
        return Some("alacritty".into());
    }
    if crate::utils::process_env::var("TILIX_ID").is_ok() {
        return Some("tilix".into());
    }

    // Windows (lines 201-210)
    if crate::utils::process_env::var("WT_SESSION").is_ok() {
        return Some("windows-terminal".into());
    }
    if crate::utils::process_env::var("SESSIONNAME").is_ok()
        && crate::utils::process_env::var("TERM").as_deref() == Ok("cygwin")
    {
        return Some("cygwin".into());
    }
    if let Ok(msystem) = crate::utils::process_env::var("MSYSTEM") {
        return Some(msystem.to_lowercase());
    }
    if crate::utils::process_env::var("ConEmuANSI").is_ok()
        || crate::utils::process_env::var("ConEmuPID").is_ok()
        || crate::utils::process_env::var("ConEmuTask").is_ok()
    {
        return Some("conemu".into());
    }

    // WSL (line 213)
    if let Ok(distro) = crate::utils::process_env::var("WSL_DISTRO_NAME") {
        return Some(format!("wsl-{distro}"));
    }

    // SSH (lines 216-218)
    if is_ssh_session() {
        return Some("ssh-session".into());
    }

    // TERM fallback (lines 222-228)
    if let Ok(term) = crate::utils::process_env::var("TERM") {
        if term.contains("alacritty") {
            return Some("alacritty".into());
        }
        if term.contains("rxvt") {
            return Some("rxvt".into());
        }
        if term.contains("termite") {
            return Some("termite".into());
        }
        return Some(term);
    }

    None
}

fn detect_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOS
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

fn is_ssh_session() -> bool {
    crate::utils::process_env::var("SSH_CONNECTION").is_ok()
        || crate::utils::process_env::var("SSH_CLIENT").is_ok()
        || crate::utils::process_env::var("SSH_TTY").is_ok()
}
