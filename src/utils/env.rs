//! Maps to: CC `utils/env.ts`.
//!
//! CC's `env` object (`env.ts:316-333`) mixes evaluation times, kept apart
//! here:
//! - import time: `isCI`, `platform` and `terminal` are computed when the
//!   module loads, before `init()` applies settings env. [`get`] holds them;
//!   `entrypoints/cli.rs` forces it in the startup window, so settings env
//!   never reaches them, as in CC;
//! - call time: `isSSH` (`env.ts:308-314`) reads the environment on every
//!   call ([`is_ssh_session`]);
//! - first call: `getGlobalClaudeFile` (`env.ts:14-26`) is memoized without a
//!   key ([`get_global_claude_file`]).
//!
//! The rest of CC's `env` (package managers, runtimes, WSL, deployment
//! environment) has no Rust caller yet.

use std::io::IsTerminal;
use std::sync::LazyLock;

use crate::utils::process_env::{self, EnvSnapshot, JsTruthy};

/// The import-time fields of CC's `env`, computed from the startup window's
/// environment.
static ENV: LazyLock<Env> = LazyLock::new(|| Env::detect(&process_env::snapshot()));

/// CC `env` (`env.ts:316-333`): its import-time fields.
pub fn get() -> &'static Env {
    &ENV
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
}

/// Maps to: CC `utils/env.ts:14-26` `getGlobalClaudeFile`, memoized without a
/// key: the first call fixes the path for the process. That is the legacy
/// `.config.json` under the config home when it exists, else
/// `.claude<suffix>.json` under `CLAUDE_CONFIG_DIR || homedir()`, the suffix
/// naming a non-production OAuth config.
///
/// Test builds compute it on every call instead: under `cargo test` many tests
/// share a process, and a path fixed by the first would send every later
/// test's writes there, the real `~/.claude.json` among them.
pub fn get_global_claude_file() -> std::path::PathBuf {
    #[cfg(not(test))]
    {
        static FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        FILE.get_or_init(global_claude_file).clone()
    }
    #[cfg(test)]
    global_claude_file()
}

/// The body of [`get_global_claude_file`].
fn global_claude_file() -> std::path::PathBuf {
    // Legacy fallback for backwards compatibility
    let legacy = crate::utils::env_utils::get_claude_config_home_dir().join(".config.json");
    if crate::utils::fs_operations::get_fs_implementation().exists_sync(&legacy) {
        return legacy;
    }
    let filename = format!(
        ".claude{}.json",
        crate::constants::oauth::file_suffix_for_oauth_config()
    );
    let env = process_env::snapshot();
    match env.var_os("CLAUDE_CONFIG_DIR").truthy() {
        Some(dir) => std::path::PathBuf::from(dir).join(filename),
        None => crate::utils::node_os::homedir().join(filename),
    }
}

#[derive(Debug)]
pub struct Env {
    pub platform: Platform,
    pub terminal: Option<String>,
    pub is_ci: bool,
}

impl Env {
    fn detect(env: &EnvSnapshot) -> Self {
        Self {
            platform: detect_platform(),
            terminal: detect_terminal(env),
            is_ci: crate::utils::env_utils::is_env_truthy(env.var("CI")),
        }
    }
}

/// Maps to: CC `env.ts:115-132` `JETBRAINS_IDES`.
pub const JETBRAINS_IDES: &[&str] = &[
    "pycharm",
    "intellij",
    "webstorm",
    "phpstorm",
    "rubymine",
    "clion",
    "goland",
    "rider",
    "datagrip",
    "appcode",
    "dataspell",
    "aqua",
    "gateway",
    "fleet",
    "jetbrains",
    "androidstudio",
];

/// Maps to: CC `env.ts:134-234` `detectTerminal`. Each `if (process.env.X)`
/// is a truthiness test, so an empty value counts as unset; `?.includes` and
/// `===` compare whatever is there.
fn detect_terminal(env: &EnvSnapshot) -> Option<String> {
    let set = |key: &str| env.var(key).truthy();

    if set("CURSOR_TRACE_ID").is_some() {
        return Some("cursor".into());
    }
    // Cursor and Windsurf under WSL have TERM_PROGRAM=vscode.
    if let Some(askpass) = env.var("VSCODE_GIT_ASKPASS_MAIN") {
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
    if let Some(bundle_id) = env.var("__CFBundleIdentifier").map(str::to_lowercase) {
        if bundle_id.contains("vscodium") {
            return Some("codium".into());
        }
        if bundle_id.contains("windsurf") {
            return Some("windsurf".into());
        }
        if bundle_id.contains("com.google.android.studio") {
            return Some("androidstudio".into());
        }
        // JetBrains IDEs in the bundle ID (`if (bundleId)`).
        if let Some(ide) = JETBRAINS_IDES.iter().find(|ide| bundle_id.contains(*ide)) {
            return Some((*ide).into());
        }
    }

    if set("VisualStudioVersion").is_some() {
        // Desktop Visual Studio, not VS Code.
        return Some("visualstudio".into());
    }

    // JetBrains terminal on Linux/Windows. CC returns 'pycharm' on every
    // platform here: macOS was handled by the bundle ID above, and the
    // finer-grained detection is `envDynamic`'s.
    if env.var("TERMINAL_EMULATOR") == Some("JetBrains-JediTerm") {
        return Some("pycharm".into());
    }

    // Specific terminals by TERM before TERM_PROGRAM.
    let term = env.var("TERM");
    if term == Some("xterm-ghostty") {
        return Some("ghostty".into());
    }
    if term.is_some_and(|term| term.contains("kitty")) {
        return Some("kitty".into());
    }

    if let Some(program) = set("TERM_PROGRAM") {
        return Some(program.into());
    }

    if set("TMUX").is_some() {
        return Some("tmux".into());
    }
    if set("STY").is_some() {
        return Some("screen".into());
    }

    // Terminal-specific variables (common on Linux).
    for (key, terminal) in [
        ("KONSOLE_VERSION", "konsole"),
        ("GNOME_TERMINAL_SERVICE", "gnome-terminal"),
        ("XTERM_VERSION", "xterm"),
        ("VTE_VERSION", "vte-based"),
        ("TERMINATOR_UUID", "terminator"),
        ("KITTY_WINDOW_ID", "kitty"),
        ("ALACRITTY_LOG", "alacritty"),
        ("TILIX_ID", "tilix"),
        // Windows-specific detection.
        ("WT_SESSION", "windows-terminal"),
    ] {
        if set(key).is_some() {
            return Some(terminal.into());
        }
    }
    if set("SESSIONNAME").is_some() && term == Some("cygwin") {
        return Some("cygwin".into());
    }
    if let Some(msystem) = set("MSYSTEM") {
        // MINGW64, MSYS2, etc.
        return Some(msystem.to_lowercase());
    }
    if set("ConEmuANSI").is_some() || set("ConEmuPID").is_some() || set("ConEmuTask").is_some() {
        return Some("conemu".into());
    }

    if let Some(distro) = set("WSL_DISTRO_NAME") {
        return Some(format!("wsl-{distro}"));
    }

    if is_ssh_session_in(env) {
        return Some("ssh-session".into());
    }

    // TERM is more universally available; special-case common identifiers.
    if let Some(term) = term.truthy() {
        if term.contains("alacritty") {
            return Some("alacritty".into());
        }
        if term.contains("rxvt") {
            return Some("rxvt".into());
        }
        if term.contains("termite") {
            return Some("termite".into());
        }
        return Some(term.into());
    }

    if !std::io::stdout().is_terminal() {
        return Some("non-interactive".into());
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

/// Maps to: CC `env.ts:308-314` `isSSHSession`, exported as `env.isSSH`.
/// Read on every call, unlike the import-time fields.
pub fn is_ssh_session() -> bool {
    is_ssh_session_in(&process_env::snapshot())
}

fn is_ssh_session_in(env: &EnvSnapshot) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .into_iter()
        .any(|key| env.var(key).truthy().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// CC `env.ts:14-26`: `.claude<suffix>.json` under `CLAUDE_CONFIG_DIR`,
    /// unless the legacy `.config.json` exists under the config home. (The
    /// memoization is production-only; see [`get_global_claude_file`].)
    #[test]
    fn global_claude_file_matches_official_legacy_and_suffix() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-global-claude-file-{}",
            uuid::Uuid::new_v4()
        ));
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _oauth = [
            EnvVarGuard::unset("CLAUDE_CODE_CUSTOM_OAUTH_URL"),
            EnvVarGuard::unset("USE_LOCAL_OAUTH"),
            EnvVarGuard::unset("USE_STAGING_OAUTH"),
        ];
        assert_eq!(global_claude_file(), config.join(".claude.json"));
        {
            let _oauth = EnvVarGuard::set(
                "CLAUDE_CODE_CUSTOM_OAUTH_URL",
                "https://claude.fedstart.com",
            );
            assert_eq!(
                global_claude_file(),
                config.join(".claude-custom-oauth.json")
            );
        }
        std::fs::write(config.join(".config.json"), "{}").unwrap();
        assert_eq!(global_claude_file(), config.join(".config.json"));
        let _ = std::fs::remove_dir_all(root);
    }

    fn terminal(pairs: &[(&str, &str)]) -> Option<String> {
        detect_terminal(&EnvSnapshot::from_pairs(pairs.iter().copied()))
    }

    /// CC `if (process.env.X)` treats an empty value as unset, so detection
    /// falls through to the next rule instead of answering.
    #[test]
    fn empty_values_fall_through_as_in_official_truthiness() {
        assert_eq!(
            terminal(&[("CURSOR_TRACE_ID", ""), ("TERM_PROGRAM", "WezTerm")]).as_deref(),
            Some("WezTerm")
        );
        assert_eq!(
            terminal(&[("TERM_PROGRAM", ""), ("TMUX", ""), ("STY", "1")]).as_deref(),
            Some("screen")
        );
        assert_eq!(
            terminal(&[("SSH_TTY", ""), ("TERM", "xterm-256color")]).as_deref(),
            Some("xterm-256color")
        );
    }

    /// CC `env.ts:147-156`: the lowercased bundle ID names Android Studio and
    /// every JetBrains IDE, in `JETBRAINS_IDES` order.
    #[test]
    fn bundle_ids_name_android_studio_and_jetbrains_ides() {
        assert_eq!(
            terminal(&[("__CFBundleIdentifier", "com.google.android.studio")]).as_deref(),
            Some("androidstudio")
        );
        assert_eq!(
            terminal(&[("__CFBundleIdentifier", "com.jetbrains.WebStorm")]).as_deref(),
            Some("webstorm")
        );
        assert_eq!(
            terminal(&[("__CFBundleIdentifier", "com.jetbrains.intellij.ce")]).as_deref(),
            Some("intellij")
        );
    }

    #[test]
    fn term_and_windows_rules_keep_official_order() {
        assert_eq!(
            terminal(&[("TERM", "xterm-kitty"), ("TERM_PROGRAM", "tmux")]).as_deref(),
            Some("kitty")
        );
        // MSYSTEM comes later, so only the cygwin rule answers "cygwin" here.
        assert_eq!(
            terminal(&[
                ("SESSIONNAME", "Console"),
                ("TERM", "cygwin"),
                ("MSYSTEM", "MINGW64")
            ])
            .as_deref(),
            Some("cygwin")
        );
        assert_eq!(
            terminal(&[("MSYSTEM", "MINGW64")]).as_deref(),
            Some("mingw64")
        );
        assert_eq!(
            terminal(&[("WSL_DISTRO_NAME", "Ubuntu"), ("TERM", "xterm")]).as_deref(),
            Some("wsl-Ubuntu")
        );
        assert_eq!(
            terminal(&[("SSH_CONNECTION", "1 2 3 4"), ("TERM", "xterm")]).as_deref(),
            Some("ssh-session")
        );
    }
}
