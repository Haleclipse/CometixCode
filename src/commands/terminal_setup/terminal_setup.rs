//! Maps to: CC `commands/terminalSetup/terminalSetup.tsx`.

use std::path::{Path, PathBuf};

use crate::components::design_system::color::{ColorType, color};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::utils::config::{load_global_config, save_global_config};
use crate::utils::env::{self, Platform};

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx` `EOL`.
pub const EOL: &str = "\n";

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `NATIVE_CSIU_TERMINALS`.
pub const NATIVE_CSIU_TERMINALS: &[(&str, &str)] = &[
    ("ghostty", "Ghostty"),
    ("kitty", "Kitty"),
    ("iTerm.app", "iTerm2"),
    ("WezTerm", "WezTerm"),
    ("WarpTerminal", "Warp"),
];

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `VSCodeKeybinding`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VSCodeKeybinding {
    pub key: String,
    pub command: String,
    pub args: VSCodeKeybindingArgs,
    pub when: String,
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `VSCodeKeybinding.args`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VSCodeKeybindingArgs {
    pub text: String,
}

/// Rust equivalent of the official
/// `installBindingsForVSCodeTerminal(editor: 'VSCode' | 'Cursor' | 'Windsurf')`
/// editor union.
/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForVSCodeTerminal` `editor` parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VSCodeFamilyEditor {
    VSCode,
    Cursor,
    Windsurf,
}

impl VSCodeFamilyEditor {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::VSCode => "VSCode",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
        }
    }

    fn user_dir_name(self) -> &'static str {
        match self {
            Self::VSCode => "Code",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
        }
    }
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `ALACRITTY_KEYBINDING` local constant inside `installBindingsForAlacritty`.
pub const ALACRITTY_KEYBINDING: &str =
    "[[keyboard.bindings]]\nkey = \"Return\"\nmods = \"Shift\"\nchars = \"\\u001B\\r\"";

fn native_csiu_display_name_from_table(
    terminal: Option<&str>,
    table: &[(&'static str, &'static str)],
) -> Option<&'static str> {
    terminal.and_then(|term| {
        table
            .iter()
            .find_map(|(key, display)| (*key == term).then_some(*display))
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `getNativeCSIuTerminalDisplayName`.
pub fn get_native_csiu_terminal_display_name_for(terminal: Option<&str>) -> Option<&'static str> {
    native_csiu_display_name_from_table(terminal, NATIVE_CSIU_TERMINALS)
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `getNativeCSIuTerminalDisplayName`.
pub fn get_native_csiu_terminal_display_name() -> Option<&'static str> {
    get_native_csiu_terminal_display_name_for(env::get().terminal.as_deref())
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `shouldOfferTerminalSetup`.
pub fn should_offer_terminal_setup_for(terminal: Option<&str>, platform: Platform) -> bool {
    matches!(
        (terminal, platform),
        (Some("Apple_Terminal"), Platform::MacOS)
    ) || matches!(
        terminal,
        Some("vscode" | "cursor" | "windsurf" | "alacritty" | "zed")
    )
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `shouldOfferTerminalSetup`.
pub fn should_offer_terminal_setup() -> bool {
    let detected = env::get();
    should_offer_terminal_setup_for(detected.terminal.as_deref(), detected.platform)
}

fn platform_terminal_suggestions(platform: Platform) -> Vec<&'static str> {
    match platform {
        Platform::MacOS => vec!["   • macOS: Apple Terminal"],
        Platform::Windows => vec!["   • Windows: Windows Terminal"],
        Platform::Linux => Vec::new(),
    }
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForVSCodeTerminal` terminal-to-editor call sites.
pub fn vscode_family_editor_for_terminal(terminal: Option<&str>) -> Option<VSCodeFamilyEditor> {
    match terminal {
        Some("vscode") => Some(VSCodeFamilyEditor::VSCode),
        Some("cursor") => Some(VSCodeFamilyEditor::Cursor),
        Some("windsurf") => Some(VSCodeFamilyEditor::Windsurf),
        _ => None,
    }
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `isVSCodeRemoteSSH`.
pub fn is_vscode_remote_ssh_for_env(askpass_main: &str, path_env: &str) -> bool {
    [".vscode-server", ".cursor-server", ".windsurf-server"]
        .iter()
        .any(|marker| askpass_main.contains(marker) || path_env.contains(marker))
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForVSCodeTerminal` `userDirPath`.
pub fn vscode_user_dir_path_for(
    home: impl AsRef<Path>,
    platform: Platform,
    editor: VSCodeFamilyEditor,
) -> PathBuf {
    let home = home.as_ref();
    let editor_dir = editor.user_dir_name();
    match platform {
        Platform::Windows => home
            .join("AppData")
            .join("Roaming")
            .join(editor_dir)
            .join("User"),
        Platform::MacOS => home
            .join("Library")
            .join("Application Support")
            .join(editor_dir)
            .join("User"),
        Platform::Linux => home.join(".config").join(editor_dir).join("User"),
    }
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForVSCodeTerminal` `keybindingsPath`.
pub fn vscode_keybindings_path_for(
    home: impl AsRef<Path>,
    platform: Platform,
    editor: VSCodeFamilyEditor,
) -> PathBuf {
    vscode_user_dir_path_for(home, platform, editor).join("keybindings.json")
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForVSCodeTerminal` `newKeybinding`.
pub fn vscode_shift_enter_keybinding() -> VSCodeKeybinding {
    VSCodeKeybinding {
        key: "shift+enter".to_string(),
        command: "workbench.action.terminal.sendSequence".to_string(),
        args: VSCodeKeybindingArgs {
            text: "\u{1b}\r".to_string(),
        },
        when: "terminalFocus".to_string(),
    }
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForAlacritty` `configPaths` construction.
pub fn alacritty_config_paths_for(
    home: impl AsRef<Path>,
    xdg_config_home: Option<&Path>,
    app_data: Option<&Path>,
    platform: Platform,
) -> Vec<PathBuf> {
    let home = home.as_ref();
    let mut config_paths = Vec::new();
    if let Some(xdg_config_home) = xdg_config_home {
        config_paths.push(xdg_config_home.join("alacritty").join("alacritty.toml"));
    } else {
        config_paths.push(
            home.join(".config")
                .join("alacritty")
                .join("alacritty.toml"),
        );
    }

    if platform == Platform::Windows {
        if let Some(app_data) = app_data {
            config_paths.push(app_data.join("alacritty").join("alacritty.toml"));
        }
    }
    config_paths
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForAlacritty` existing keybinding check.
pub fn alacritty_config_has_shift_enter_binding(config_content: &str) -> bool {
    config_content.contains("mods = \"Shift\"") && config_content.contains("key = \"Return\"")
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForAlacritty` `updatedContent` assembly.
pub fn append_alacritty_keybinding(config_content: &str) -> String {
    let mut updated_content = config_content.to_string();
    if !config_content.is_empty() && !config_content.ends_with('\n') {
        updated_content.push('\n');
    }
    updated_content.push('\n');
    updated_content.push_str(ALACRITTY_KEYBINDING);
    updated_content.push('\n');
    updated_content
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForZed` `keymapPath`.
pub fn zed_keymap_path_for(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref()
        .join(".config")
        .join("zed")
        .join("keymap.json")
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForZed` existing keybinding check.
pub fn zed_keymap_has_shift_enter_binding(keymap_content: &str) -> bool {
    keymap_content.contains("shift-enter")
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `installBindingsForZed` pushed keymap entry.
pub fn zed_shift_enter_keymap_entry() -> serde_json::Value {
    json!({
        "context": "Terminal",
        "bindings": {
            "shift-enter": ["terminal::SendText", "\u{1b}\r"],
        },
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `isShiftEnterKeyBindingInstalled`.
pub fn is_shift_enter_key_binding_installed() -> bool {
    load_global_config().shift_enter_key_binding_installed == Some(true)
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `hasUsedBackslashReturn`.
pub fn has_used_backslash_return() -> bool {
    load_global_config().has_used_backslash_return == Some(true)
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx`
/// `markBackslashReturnUsed`.
///
pub fn mark_backslash_return_used() -> anyhow::Result<()> {
    if has_used_backslash_return() {
        return Ok(());
    }
    save_global_config(|config| {
        config.has_used_backslash_return = Some(true);
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#formatPathLink:82-89`.
fn format_path_link(path: &Path) -> String {
    let text = path.to_string_lossy();
    if !iocraft::prelude::supports_hyperlinks() {
        return text.into_owned();
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let url = url::Url::from_file_path(absolute).expect("absolute configuration path");
    format!("\x1b]8;;{url}\x07{text}\x1b]8;;\x07")
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#call:174-222`; Result transports onDone
/// through the existing typed command executable pointer.
pub async fn call(theme: crate::utils::theme::ThemeName) -> anyhow::Result<String> {
    let detected = env::get();
    if let Some(message) =
        terminal_setup_output_for(detected.terminal.as_deref(), detected.platform)
    {
        return Ok(message);
    }
    setup_terminal(theme).await
}

// Pure projection of call's two early onDone branches; supported terminals
// return None and continue into the real installer.
pub fn terminal_setup_output_for(terminal: Option<&str>, platform: Platform) -> Option<String> {
    if let Some(display) = get_native_csiu_terminal_display_name_for(terminal) {
        return Some(format!(
            "Shift+Enter is natively supported in {display}.\n\nNo configuration needed. Just use Shift+Enter to add newlines."
        ));
    }
    if should_offer_terminal_setup_for(terminal, platform) {
        return None;
    }
    let name = terminal
        .filter(|value| !value.is_empty())
        .unwrap_or("your current terminal");
    let terminals = platform_terminal_suggestions(platform)
        .into_iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let note = chalk::Chalk::new()
        .dim()
        .apply("Note: You can already use backslash (\\\\) + return to add newlines.");
    let native_note = chalk::Chalk::new()
        .dim()
        .apply("Note: iTerm2, WezTerm, Ghostty, Kitty, and Warp support Shift+Enter natively.");
    Some(format!(
        "Terminal setup cannot be run from {name}.\n\nThis command configures a convenient Shift+Enter shortcut for multi-line prompts.\n{note}\n\nTo set up the shortcut (optional):\n1. Exit tmux/screen temporarily\n2. Run /terminal-setup directly in one of these terminals:\n{terminals}   • IDE: VSCode, Cursor, Windsurf, Zed\n   • Other: Alacritty\n3. Return to tmux/screen - settings will persist\n\n{native_note}"
    ))
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#setupTerminal:105-154`.
pub async fn setup_terminal(theme: crate::utils::theme::ThemeName) -> anyhow::Result<String> {
    let detected = env::get();
    let result = match detected.terminal.as_deref() {
        Some("Apple_Terminal") => enable_option_as_meta_for_terminal(theme).await?,
        Some("vscode") => {
            install_bindings_for_vscode_terminal(VSCodeFamilyEditor::VSCode, theme).await?
        }
        Some("cursor") => {
            install_bindings_for_vscode_terminal(VSCodeFamilyEditor::Cursor, theme).await?
        }
        Some("windsurf") => {
            install_bindings_for_vscode_terminal(VSCodeFamilyEditor::Windsurf, theme).await?
        }
        Some("alacritty") => install_bindings_for_alacritty(theme).await?,
        Some("zed") => install_bindings_for_zed(theme).await?,
        _ => String::new(),
    };
    save_global_config(|current| match detected.terminal.as_deref() {
        Some("vscode" | "cursor" | "windsurf" | "alacritty" | "zed") => {
            current.shift_enter_key_binding_installed = Some(true)
        }
        Some("Apple_Terminal") => current.option_as_meta_key_installed = Some(true),
        _ => {}
    })?;
    crate::project_onboarding_state::maybe_mark_project_onboarding_complete()?;
    // The source snapshot is external: its `"external" === 'ant'` shell
    // completion branch is inactive (completionCache owner is not ported).
    Ok(result)
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#installBindingsForVSCodeTerminal:231-338`.
pub async fn install_bindings_for_vscode_terminal(
    editor: VSCodeFamilyEditor,
    theme: crate::utils::theme::ThemeName,
) -> anyhow::Result<String> {
    let theme = crate::utils::theme::get_theme(theme);
    let editor_name = editor.display_name();
    if is_vscode_remote_ssh_for_env(
        &crate::utils::process_env::var("VSCODE_GIT_ASKPASS_MAIN").unwrap_or_default(),
        &crate::utils::process_env::var("PATH").unwrap_or_default(),
    ) {
        let warning = color(Some(theme.warning), ColorType::Foreground)(&format!(
            "Cannot install keybindings from a remote {editor_name} session."
        ));
        let snippet = chalk::Chalk::new().dim().apply("[\n  {\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": { \"text\": \"\\u001b\\r\" },\n    \"when\": \"terminalFocus\"\n  }\n]");
        return Ok(format!(
            "{warning}\n\n{editor_name} keybindings must be installed on your local machine, not the remote server.\n\nTo install the Shift+Enter keybinding:\n1. Open {editor_name} on your local machine (not connected to remote)\n2. Open the Command Palette (Cmd/Ctrl+Shift+P) → \"Preferences: Open Keyboard Shortcuts (JSON)\"\n3. Add this keybinding (the file must be a JSON array):\n\n{snippet}\n"
        ));
    }
    let home = crate::utils::node_os::homedir();
    let user_dir = vscode_user_dir_path_for(home, env::get().platform, editor);
    let path = user_dir.join("keybindings.json");
    let result: anyhow::Result<String> = async {
        tokio::fs::create_dir_all(&user_dir).await?;
        let mut content = "[]".to_string();
        let mut bindings = None;
        let mut exists = false;
        match tokio::fs::read(&path).await {
            Ok(bytes) => { content = String::from_utf8_lossy(&bytes).into_owned(); exists = true; bindings = crate::utils::json::safe_parse_jsonc(&content); },
            Err(error) if crate::utils::errors::is_fs_inaccessible(&error) => {},
            Err(error) => return Err(error.into()),
        }
        if exists {
            let mut bytes = [0u8; 4]; getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            let suffix = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            let backup = PathBuf::from(format!("{}.{suffix}.bak", path.display()));
            if tokio::fs::copy(&path, &backup).await.is_err() {
                return Ok(format!("{}\n{}\n{}\n", color(Some(theme.warning), ColorType::Foreground)(&format!("Error backing up existing {editor_name} terminal keybindings. Bailing out.")), chalk::Chalk::new().dim().apply(&format!("See {}", format_path_link(&path))), chalk::Chalk::new().dim().apply(&format!("Backup path: {}", format_path_link(&backup)))));
            }
        }
        let mut found = false;
        if let Some(bindings) = bindings.as_ref().filter(|value| !value.is_null()) {
            let length = bindings.array_find_length().map_err(|()| anyhow::anyhow!("keybindings.find is not a function"))?;
            for index in 0..length {
                // Native Array.find reads every index (including holes) and
                // its callback reads ordinary, possibly inherited properties.
                let binding = bindings.array_find_item(index).ok_or_else(|| anyhow::anyhow!("Cannot read properties of undefined (reading 'key')"))?;
                if binding.is_null() { anyhow::bail!("Cannot read properties of null (reading 'key')"); }
                if binding.get_property("key").and_then(|v| v.as_str()) == Some("shift+enter") && binding.get_property("command").and_then(|v| v.as_str()) == Some("workbench.action.terminal.sendSequence") && binding.get_property("when").and_then(|v| v.as_str()) == Some("terminalFocus") { found = true; break; }
            }
        }
        if found {
            return Ok(format!("{}\n{}\n", color(Some(theme.warning), ColorType::Foreground)(&format!("Found existing {editor_name} terminal Shift+Enter key binding. Remove it to continue.")), chalk::Chalk::new().dim().apply(&format!("See {}", format_path_link(&path)))));
        }
        let binding = serde_json::to_value(vscode_shift_enter_keybinding())?;
        let updated = crate::utils::json::add_item_to_jsonc_array(&content, &binding);
        tokio::fs::write(&path, updated).await?;
        Ok(format!("{}\n{}\n", color(Some(theme.success), ColorType::Foreground)(&format!("Installed {editor_name} terminal Shift+Enter key binding")), chalk::Chalk::new().dim().apply(&format!("See {}", format_path_link(&path)))))
    }.await;
    result.map_err(|error| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        anyhow::anyhow!("Failed to install {editor_name} terminal Shift+Enter key binding")
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#enableOptionAsMetaForProfile:340-373`.
async fn enable_option_as_meta_for_profile(profile_name: &str) -> bool {
    #[cfg(not(test))]
    use crate::utils::apple_terminal_backup::get_terminal_plist_path;
    #[cfg(test)]
    use tests::apple_installer::get_terminal_plist_path;
    let path = get_terminal_plist_path();
    let path = path.to_string_lossy();
    let add = format!("Add :'Window Settings':'{profile_name}':useOptionAsMetaKey bool true");
    let added =
        terminal_setup_exec_file_no_throw("/usr/libexec/PlistBuddy", &["-c", &add, &path]).await;
    if added.code != 0 {
        let set = format!("Set :'Window Settings':'{profile_name}':useOptionAsMetaKey true");
        let changed =
            terminal_setup_exec_file_no_throw("/usr/libexec/PlistBuddy", &["-c", &set, &path])
                .await;
        if changed.code != 0 {
            let error = crate::utils::log::LogError::new(format!(
                "Failed to enable Option as Meta key for Terminal.app profile: {profile_name}"
            ));
            #[cfg(test)]
            tests::apple_installer::log(&error);
            crate::utils::log::log_error(error);
            return false;
        }
    }
    true
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#disableAudioBellForProfile:375-403`.
async fn disable_audio_bell_for_profile(profile_name: &str) -> bool {
    #[cfg(not(test))]
    use crate::utils::apple_terminal_backup::get_terminal_plist_path;
    #[cfg(test)]
    use tests::apple_installer::get_terminal_plist_path;
    let path = get_terminal_plist_path();
    let path = path.to_string_lossy();
    let add = format!("Add :'Window Settings':'{profile_name}':Bell bool false");
    let added =
        terminal_setup_exec_file_no_throw("/usr/libexec/PlistBuddy", &["-c", &add, &path]).await;
    if added.code != 0 {
        let set = format!("Set :'Window Settings':'{profile_name}':Bell false");
        let changed =
            terminal_setup_exec_file_no_throw("/usr/libexec/PlistBuddy", &["-c", &set, &path])
                .await;
        if changed.code != 0 {
            let error = crate::utils::log::LogError::new(format!(
                "Failed to disable audio bell for Terminal.app profile: {profile_name}"
            ));
            #[cfg(test)]
            tests::apple_installer::log(&error);
            crate::utils::log::log_error(error);
            return false;
        }
    }
    true
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#enableOptionAsMetaForTerminal:405-499`.
pub async fn enable_option_as_meta_for_terminal(
    theme_name: crate::utils::theme::ThemeName,
) -> anyhow::Result<String> {
    use crate::utils::apple_terminal_backup::RestoreResult;
    #[cfg(not(test))]
    use crate::utils::apple_terminal_backup::{
        backup_terminal_preferences, check_and_restore_terminal_backup,
        mark_terminal_setup_complete,
    };
    #[cfg(test)]
    use tests::apple_installer::{
        backup_terminal_preferences, check_and_restore_terminal_backup,
        mark_terminal_setup_complete,
    };
    let result: anyhow::Result<String> = async {
        if backup_terminal_preferences().await.is_none() {
            anyhow::bail!("Failed to create backup of Terminal.app preferences, bailing out");
        }
        let default_profile = terminal_setup_exec_file_no_throw("defaults", &["read", "com.apple.Terminal", "Default Window Settings"]).await;
        if default_profile.code != 0 || default_profile.stdout.trim().is_empty() {
            anyhow::bail!("Failed to read default Terminal.app profile");
        }
        let startup_profile = terminal_setup_exec_file_no_throw("defaults", &["read", "com.apple.Terminal", "Startup Window Settings"]).await;
        if startup_profile.code != 0 || startup_profile.stdout.trim().is_empty() {
            anyhow::bail!("Failed to read startup Terminal.app profile");
        }
        let default_name = default_profile.stdout.trim();
        let option_enabled = enable_option_as_meta_for_profile(default_name).await;
        let bell_disabled = disable_audio_bell_for_profile(default_name).await;
        let mut was_any_profile_updated = option_enabled || bell_disabled;
        let startup_name = startup_profile.stdout.trim();
        if startup_name != default_name {
            let option_enabled = enable_option_as_meta_for_profile(startup_name).await;
            let bell_disabled = disable_audio_bell_for_profile(startup_name).await;
            if option_enabled || bell_disabled { was_any_profile_updated = true; }
        }
        if !was_any_profile_updated {
            anyhow::bail!("Failed to enable Option as Meta key or disable audio bell for any Terminal.app profile");
        }
        terminal_setup_exec_file_no_throw("killall", &["cfprefsd"]).await;
        mark_terminal_setup_complete()?;
        let theme = crate::utils::theme::get_theme(theme_name);
        Ok(format!("{}\n{}\n{}\n{}\n{}\n", color(Some(theme.success), ColorType::Foreground)("Configured Terminal.app settings:"), color(Some(theme.success), ColorType::Foreground)("- Enabled \"Use Option as Meta key\""), color(Some(theme.success), ColorType::Foreground)("- Switched to visual bell"), chalk::Chalk::new().dim().apply("Option+Enter will now enter a newline."), chalk::Chalk::new().dim().apply(&format!("You must restart Terminal.app for changes to take effect. {}", theme_name.setting_value()))))
    }.await;
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            let error = crate::utils::log::LogError::new(error.to_string());
            #[cfg(test)]
            tests::apple_installer::log(&error);
            crate::utils::log::log_error(error);
            let restore = check_and_restore_terminal_backup().await?;
            let prefix = "Failed to enable Option as Meta key for Terminal.app.";
            match restore {
                RestoreResult::Restored => {
                    anyhow::bail!("{prefix} Your settings have been restored from backup.")
                }
                RestoreResult::Failed { backup_path } => anyhow::bail!(
                    "{prefix} Restoring from backup failed, try manually with: defaults import com.apple.Terminal {backup_path}"
                ),
                RestoreResult::NoBackup => {
                    anyhow::bail!("{prefix} No backup was available to restore from.")
                }
            }
        }
    }
}

// Policy-free async carrier for the source's imported execFileNoThrow calls.
// Captures the current directory before scheduling blocking subprocess work.
async fn terminal_setup_exec_file_no_throw(
    program: &str,
    args: &[&str],
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    #[cfg(test)]
    if let Some(output) = tests::apple_installer::exec(program, args) {
        return output;
    }
    let program = program.to_owned();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let cwd =
        std::env::current_dir().unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd());
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::utils::exec_file_no_throw::exec_file_no_throw_with_cwd(
            &program,
            &args,
            std::time::Duration::from_secs(600),
            Some(&cwd),
            true,
        )
    })
    .await
    .unwrap_or_else(|_| crate::utils::exec_file_no_throw::ExecFileOutput {
        code: 1,
        stdout: String::new(),
        stderr: String::new(),
        error: None,
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#installBindingsForAlacritty:501-608`.
pub async fn install_bindings_for_alacritty(
    theme: crate::utils::theme::ThemeName,
) -> anyhow::Result<String> {
    let theme = crate::utils::theme::get_theme(theme);
    let home = crate::utils::node_os::homedir();
    let xdg = crate::utils::process_env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let app_data = crate::utils::process_env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let paths = alacritty_config_paths_for(
        home,
        xdg.as_deref(),
        app_data.as_deref(),
        env::get().platform,
    );
    let mut selected = None;
    let mut content = String::new();
    let mut exists = false;
    for path in &paths {
        match tokio::fs::read(path).await {
            Ok(bytes) => {
                content = String::from_utf8_lossy(&bytes).into_owned();
                selected = Some(path.clone());
                exists = true;
                break;
            }
            Err(error) if crate::utils::errors::is_fs_inaccessible(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let path = selected
        .or_else(|| paths.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("No valid config path found for Alacritty"))?;
    let result: anyhow::Result<String> = async {
        if exists {
            if alacritty_config_has_shift_enter_binding(&content) {
                return Ok(format!(
                    "{}\n{}\n",
                    color(Some(theme.warning), ColorType::Foreground)(
                        "Found existing Alacritty Shift+Enter key binding. Remove it to continue."
                    ),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("See {}", format_path_link(&path)))
                ));
            }
            let mut bytes = [0u8; 4];
            getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            let suffix = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let backup = PathBuf::from(format!("{}.{suffix}.bak", path.display()));
            if tokio::fs::copy(&path, &backup).await.is_err() {
                return Ok(format!(
                    "{}\n{}\n{}\n",
                    color(Some(theme.warning), ColorType::Foreground)(
                        "Error backing up existing Alacritty config. Bailing out."
                    ),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("See {}", format_path_link(&path))),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("Backup path: {}", format_path_link(&backup)))
                ));
            }
        } else {
            tokio::fs::create_dir_all(path.parent().expect("configuration parent")).await?;
        }
        tokio::fs::write(&path, append_alacritty_keybinding(&content)).await?;
        Ok(format!(
            "{}\n{}\n{}\n",
            color(Some(theme.success), ColorType::Foreground)(
                "Installed Alacritty Shift+Enter key binding"
            ),
            color(Some(theme.success), ColorType::Foreground)(
                "You may need to restart Alacritty for changes to take effect"
            ),
            chalk::Chalk::new()
                .dim()
                .apply(&format!("See {}", format_path_link(&path)))
        ))
    }
    .await;
    result.map_err(|error| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        anyhow::anyhow!("Failed to install Alacritty Shift+Enter key binding")
    })
}

/// Maps to: CC `commands/terminalSetup/terminalSetup.tsx#installBindingsForZed:610-692`.
pub async fn install_bindings_for_zed(
    theme: crate::utils::theme::ThemeName,
) -> anyhow::Result<String> {
    let theme = crate::utils::theme::get_theme(theme);
    let home = crate::utils::node_os::homedir();
    let path = zed_keymap_path_for(home);
    let result: anyhow::Result<String> = async {
        tokio::fs::create_dir_all(path.parent().expect("configuration parent")).await?;
        let mut content = "[]".to_string();
        let mut exists = false;
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                content = String::from_utf8_lossy(&bytes).into_owned();
                exists = true;
            }
            Err(error) if crate::utils::errors::is_fs_inaccessible(&error) => {}
            Err(error) => return Err(error.into()),
        }
        if exists {
            if zed_keymap_has_shift_enter_binding(&content) {
                return Ok(format!(
                    "{}\n{}\n",
                    color(Some(theme.warning), ColorType::Foreground)(
                        "Found existing Zed Shift+Enter key binding. Remove it to continue."
                    ),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("See {}", format_path_link(&path)))
                ));
            }
            let mut bytes = [0u8; 4];
            getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            let suffix = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let backup = PathBuf::from(format!("{}.{suffix}.bak", path.display()));
            if tokio::fs::copy(&path, &backup).await.is_err() {
                return Ok(format!(
                    "{}\n{}\n{}\n",
                    color(Some(theme.warning), ColorType::Foreground)(
                        "Error backing up existing Zed keymap. Bailing out."
                    ),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("See {}", format_path_link(&path))),
                    chalk::Chalk::new()
                        .dim()
                        .apply(&format!("Backup path: {}", format_path_link(&backup)))
                ));
            }
        }
        let mut keymap = crate::utils::slow_operations::json_parse(&content)
            .ok()
            .and_then(crate::utils::json::JsoncValue::into_array)
            .unwrap_or_default();
        keymap.push(crate::utils::json::JsoncValue::from_json(
            zed_shift_enter_keymap_entry(),
        ));
        tokio::fs::write(
            &path,
            format!(
                "{}\n",
                crate::utils::slow_operations::json_stringify(
                    &crate::utils::json::JsoncValue::array(keymap),
                    2
                )
            ),
        )
        .await?;
        Ok(format!(
            "{}\n{}\n",
            color(Some(theme.success), ColorType::Foreground)(
                "Installed Zed Shift+Enter key binding"
            ),
            chalk::Chalk::new()
                .dim()
                .apply(&format!("See {}", format_path_link(&path)))
        ))
    }
    .await;
    result.map_err(|error| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        anyhow::anyhow!("Failed to install Zed Shift+Enter key binding")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::GlobalConfig;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};

    #[test]
    fn terminal_setup_native_terminal_matches_official_on_done_output() {
        let output = terminal_setup_output_for(Some("kitty"), Platform::Linux).unwrap();

        assert!(output.contains("Shift+Enter is natively supported in Kitty"));
        assert!(output.contains("No configuration needed"));
        assert!(!output.contains("Esc to close"));
        assert!(!output.contains("UI-only CometixCode build"));
    }

    #[test]
    fn terminal_setup_unsupported_terminal_uses_official_guidance_shape() {
        let output = terminal_setup_output_for(Some("tmux"), Platform::MacOS).unwrap();

        assert!(output.contains("Terminal setup cannot be run from tmux"));
        assert!(output.contains("This command configures a convenient Shift+Enter shortcut"));
        assert!(output.contains("Exit tmux/screen temporarily"));
        assert!(output.contains("macOS: Apple Terminal"));
        assert!(output.contains("iTerm2, WezTerm, Ghostty, Kitty, and Warp"));
        assert!(!output.contains("normally configures"));
        assert!(!output.contains("UI-only CometixCode build"));
    }

    #[test]
    fn terminal_setup_detection_helpers_match_official_supported_terminal_sets() {
        assert_eq!(
            get_native_csiu_terminal_display_name_for(Some("WarpTerminal")),
            Some("Warp")
        );
        assert_eq!(
            get_native_csiu_terminal_display_name_for(Some("Apple_Terminal")),
            None
        );
        assert!(should_offer_terminal_setup_for(
            Some("Apple_Terminal"),
            Platform::MacOS
        ));
        assert!(!should_offer_terminal_setup_for(
            Some("Apple_Terminal"),
            Platform::Linux
        ));
        for terminal in ["vscode", "cursor", "windsurf", "alacritty", "zed"] {
            assert!(should_offer_terminal_setup_for(
                Some(terminal),
                Platform::Linux
            ));
        }
        assert!(!should_offer_terminal_setup_for(
            Some("kitty"),
            Platform::Linux
        ));
    }

    #[test]
    fn terminal_setup_remote_ssh_detection_matches_official_markers() {
        assert!(is_vscode_remote_ssh_for_env(
            "/home/me/.vscode-server/bin/askpass.sh",
            ""
        ));
        assert!(is_vscode_remote_ssh_for_env(
            "",
            "/tmp/.windsurf-server/bin:/usr/bin"
        ));
        assert!(!is_vscode_remote_ssh_for_env(
            "/usr/bin/askpass",
            "/usr/bin"
        ));
    }

    #[test]
    fn terminal_setup_vscode_paths_and_keybinding_match_official_shape() {
        assert_eq!(
            vscode_family_editor_for_terminal(Some("windsurf")),
            Some(VSCodeFamilyEditor::Windsurf)
        );
        assert_eq!(
            vscode_keybindings_path_for(
                "/Users/alice",
                Platform::MacOS,
                VSCodeFamilyEditor::VSCode
            ),
            PathBuf::from("/Users/alice/Library/Application Support/Code/User/keybindings.json")
        );
        assert_eq!(
            vscode_keybindings_path_for("/home/alice", Platform::Linux, VSCodeFamilyEditor::Cursor),
            PathBuf::from("/home/alice/.config/Cursor/User/keybindings.json")
        );
        assert_eq!(
            vscode_keybindings_path_for(
                "C:/Users/Alice",
                Platform::Windows,
                VSCodeFamilyEditor::Windsurf
            ),
            PathBuf::from("C:/Users/Alice/AppData/Roaming/Windsurf/User/keybindings.json")
        );

        let binding = vscode_shift_enter_keybinding();
        assert_eq!(binding.args.text, "\u{1b}\r");
    }

    #[test]
    fn terminal_setup_alacritty_paths_detection_and_append_match_official_shape() {
        assert_eq!(
            alacritty_config_paths_for(
                "/home/alice",
                Some(Path::new("/xdg")),
                None,
                Platform::Linux
            ),
            vec![PathBuf::from("/xdg/alacritty/alacritty.toml")]
        );
        assert_eq!(
            alacritty_config_paths_for(
                "C:/Users/Alice",
                None,
                Some(Path::new("C:/Users/Alice/AppData/Roaming")),
                Platform::Windows
            ),
            vec![
                PathBuf::from("C:/Users/Alice/.config/alacritty/alacritty.toml"),
                PathBuf::from("C:/Users/Alice/AppData/Roaming/alacritty/alacritty.toml"),
            ]
        );

        assert!(alacritty_config_has_shift_enter_binding(
            "key = \"Return\"\nmods = \"Shift\""
        ));
        assert!(!alacritty_config_has_shift_enter_binding("key = \"A\""));

        assert_eq!(
            append_alacritty_keybinding("[window]\nopacity = 1"),
            "[window]\nopacity = 1\n\n[[keyboard.bindings]]\nkey = \"Return\"\nmods = \"Shift\"\nchars = \"\\u001B\\r\"\n"
        );
    }

    #[test]
    fn terminal_setup_zed_keymap_boundary_matches_official_shape() {
        assert_eq!(
            zed_keymap_path_for("/Users/alice"),
            PathBuf::from("/Users/alice/.config/zed/keymap.json")
        );
        assert!(zed_keymap_has_shift_enter_binding(
            r#"[{"context":"Terminal","bindings":{"shift-enter":["terminal::SendText",""]}}]"#
        ));
        assert!(!zed_keymap_has_shift_enter_binding(r#"[{"bindings":{}}]"#));
        assert_eq!(
            zed_shift_enter_keymap_entry(),
            json!({
                "context": "Terminal",
                "bindings": {
                    "shift-enter": ["terminal::SendText", "\u{1b}\r"],
                },
            })
        );
    }

    #[test]
    fn terminal_setup_global_config_keys_match_official_names() {
        let config: GlobalConfig = serde_json::from_str(
            r#"{
              "shiftEnterKeyBindingInstalled": true,
              "optionAsMetaKeyInstalled": true,
              "hasUsedBackslashReturn": true
            }"#,
        )
        .expect("global config should deserialize official terminal setup keys");

        assert_eq!(config.shift_enter_key_binding_installed, Some(true));
        assert_eq!(config.option_as_meta_key_installed, Some(true));
        assert_eq!(config.has_used_backslash_return, Some(true));
    }
    // Real files below are isolated import fixtures for os.homedir/XDG, not
    // user terminal preferences. nextest gives each test its own process.
    /// Points the home directory (`HOME`, and `USERPROFILE`/`APPDATA` for
    /// Windows), the XDG config root and PATH at a scratch directory. The
    /// writes go through the carrier, which the installers read, under the
    /// env lock. Fields drop in order: the variables are restored before
    /// the lock is released.
    struct InstallerFixture {
        root: PathBuf,
        _vars: Vec<EnvVarGuard>,
        _lock: TestEnvGuard<'static>,
    }
    impl InstallerFixture {
        fn new() -> Self {
            let lock = TEST_ENV_LOCK.lock().unwrap();
            let root = std::env::temp_dir()
                .join(format!("cometix-terminal-setup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let vars = vec![
                EnvVarGuard::set("HOME", &root),
                EnvVarGuard::set("USERPROFILE", &root),
                EnvVarGuard::set("APPDATA", root.join("AppData").join("Roaming")),
                EnvVarGuard::set("XDG_CONFIG_HOME", root.join(".config")),
                EnvVarGuard::unset("VSCODE_GIT_ASKPASS_MAIN"),
                EnvVarGuard::set("PATH", "/usr/bin:/bin"),
            ];
            chalk::set_stdout_level(0);
            Self {
                root,
                _vars: vars,
                _lock: lock,
            }
        }
        fn put(&self, path: &Path, value: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value).unwrap();
        }
        fn backups(&self, path: &Path) -> Vec<PathBuf> {
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "bak"))
                .collect()
        }
    }
    impl Drop for InstallerFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn terminal_setup_alacritty_installer_matches_official_write_backup_and_duplicate() {
        let fixture = InstallerFixture::new();
        let path = fixture.root.join(".config/alacritty/alacritty.toml");
        fixture.put(&path, "[window]\nopacity = 1");
        let output = install_bindings_for_alacritty(crate::utils::theme::ThemeName::Dark)
            .await
            .unwrap();
        // CC terminalSetup.tsx:551-604; Bun oracle alacritty-existing and
        // alacritty-duplicate. Duplicate returns BEFORE a second backup.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[window]\nopacity = 1\n\n[[keyboard.bindings]]\nkey = \"Return\"\nmods = \"Shift\"\nchars = \"\\u001B\\r\"\n"
        );
        assert!(output.starts_with("Installed Alacritty Shift+Enter key binding\n"));
        let backups = fixture.backups(&path);
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            "[window]\nopacity = 1"
        );
        let suffix = backups[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("alacritty.toml.")
            .unwrap()
            .strip_suffix(".bak")
            .unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert!(
            install_bindings_for_alacritty(crate::utils::theme::ThemeName::Dark)
                .await
                .unwrap()
                .starts_with("Found existing Alacritty Shift+Enter key binding.")
        );
        assert_eq!(fixture.backups(&path), backups);
    }

    #[tokio::test]
    async fn terminal_setup_zed_installer_matches_official_strict_json_and_duplicate() {
        let fixture = InstallerFixture::new();
        let path = zed_keymap_path_for(&fixture.root);
        fixture.put(&path, "[//comment\n{}]");
        let output = install_bindings_for_zed(crate::utils::theme::ThemeName::Dark)
            .await
            .unwrap();
        // CC terminalSetup.tsx:653-680: unlike VSCode, JSONC is discarded by
        // strict jsonParse; backup still retains the exact invalid content.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[\n  {\n    \"context\": \"Terminal\",\n    \"bindings\": {\n      \"shift-enter\": [\n        \"terminal::SendText\",\n        \"\\u001b\\r\"\n      ]\n    }\n  }\n]\n"
        );
        assert!(output.starts_with("Installed Zed Shift+Enter key binding\n"));
        let backups = fixture.backups(&path);
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            "[//comment\n{}]"
        );
        assert!(
            install_bindings_for_zed(crate::utils::theme::ThemeName::Dark)
                .await
                .unwrap()
                .starts_with("Found existing Zed Shift+Enter key binding.")
        );
        assert_eq!(fixture.backups(&path), backups);
    }

    #[tokio::test]
    async fn terminal_setup_zed_installer_matches_official_js_numbers_and_key_order() {
        let fixture = InstallerFixture::new();
        let path = zed_keymap_path_for(&fixture.root);
        fixture.put(
            &path,
            r#"[{"value":1.0,"negative":-0,"large":9007199254740993,"10":10,"2":2}]"#,
        );
        install_bindings_for_zed(crate::utils::theme::ThemeName::Dark)
            .await
            .unwrap();
        // CC terminalSetup.tsx:660/679 uses JS jsonParse/jsonStringify.
        // Actual Bun zed-numbers oracle: Number is binary64; numeric own keys
        // precede insertion-ordered keys, including -0 -> 0 and 1.0 -> 1.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("[\n  {\n    \"2\": 2,\n    \"10\": 10,\n    \"value\": 1,\n    \"negative\": 0,\n    \"large\": 9007199254740992\n  },\n"));
    }

    #[tokio::test]
    async fn terminal_setup_vscode_installer_matches_official_jsonc_and_backup_before_duplicate() {
        let fixture = InstallerFixture::new();
        let path = vscode_keybindings_path_for(
            &fixture.root,
            env::get().platform,
            VSCodeFamilyEditor::VSCode,
        );
        fixture.put(&path, "[\n // keep\n {\"key\":\"a\"}\n]");
        install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        // CC terminalSetup.tsx:282-330; original addItemToJSONCArray preserves
        // comments. The backup happens even on duplicate (args not checked).
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("// keep"));
        let parsed = crate::utils::json::safe_parse_jsonc(&written)
            .unwrap()
            .to_json();
        assert_eq!(parsed[0], json!({"key":"a"}));
        assert_eq!(
            parsed[1],
            serde_json::to_value(vscode_shift_enter_keybinding()).unwrap()
        );
        assert_eq!(fixture.backups(&path).len(), 1);
        fixture.put(&path, "[{\"key\":\"shift+enter\",\"command\":\"workbench.action.terminal.sendSequence\",\"when\":\"terminalFocus\"}]");
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        assert!(output.starts_with("Found existing VSCode terminal Shift+Enter key binding."));
        assert_eq!(fixture.backups(&path).len(), 2);
    }

    #[tokio::test]
    async fn terminal_setup_vscode_installer_matches_official_invalid_array_failure() {
        let fixture = InstallerFixture::new();
        let path = vscode_keybindings_path_for(
            &fixture.root,
            env::get().platform,
            VSCodeFamilyEditor::VSCode,
        );
        for content in ["{}", "[null]"] {
            fixture.put(&path, content);
            // CC terminalSetup.tsx:297 find/property access throws AFTER
            // backup, then generic installer failure; never silently replaces.
            let error = install_bindings_for_vscode_terminal(
                VSCodeFamilyEditor::VSCode,
                crate::utils::theme::ThemeName::Dark,
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Failed to install VSCode terminal Shift+Enter key binding"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
        assert_eq!(fixture.backups(&path).len(), 2);
    }

    #[tokio::test]
    async fn terminal_setup_vscode_installer_matches_official_number_and_inherited_properties() {
        let fixture = InstallerFixture::new();
        let path = vscode_keybindings_path_for(
            &fixture.root,
            env::get().platform,
            VSCodeFamilyEditor::VSCode,
        );
        fixture.put(&path, "[1e999]");
        // CC terminalSetup.tsx:297-303 reads ordinary JS properties: an
        // Infinity primitive is not null and simply has no matching key.
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        assert!(output.starts_with("Installed VSCode terminal Shift+Enter key binding"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("1e999"));
        let prototype = r#"[{"__proto__":{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","when":"terminalFocus"}}]"#;
        fixture.put(&path, prototype);
        // Real Bun vscode-prototype oracle: jsonc-parser object assignment
        // preserves this prototype and find observes inherited properties.
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        assert!(output.starts_with("Found existing VSCode terminal Shift+Enter key binding."));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), prototype);
        assert_eq!(fixture.backups(&path).len(), 2);
    }

    #[tokio::test]
    async fn terminal_setup_zed_installer_matches_official_lossless_js_parse_and_stringify() {
        let fixture = InstallerFixture::new();
        let path = zed_keymap_path_for(&fixture.root);
        let content = r#"[1e999,"\ud800",{"__proto__":{"keep":1},"\udc00":2}]"#;
        fixture.put(&path, content);
        install_bindings_for_zed(crate::utils::theme::ThemeName::Dark)
            .await
            .unwrap();
        // CC terminalSetup.tsx:660-679 JSON.parse accepts this valid JSON;
        // JSON.stringify keeps lone surrogate escapes and own __proto__ keys.
        // Actual Bun zed-lossless oracle, no lossy serde projection in between.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("[\n  null,\n  \"\\ud800\",\n  {\n    \"__proto__\": {\n      \"keep\": 1\n    },\n    \"\\udc00\": 2\n  },\n"));
        assert!(!written.contains('\u{fffd}'));
        assert_eq!(
            std::fs::read_to_string(&fixture.backups(&path)[0]).unwrap(),
            content
        );
    }

    #[tokio::test]
    async fn terminal_setup_vscode_installer_matches_official_find_receiver_and_holes() {
        let fixture = InstallerFixture::new();
        let path = vscode_keybindings_path_for(
            &fixture.root,
            env::get().platform,
            VSCodeFamilyEditor::VSCode,
        );
        let inherited = r#"{"__proto__":[{"key":"shift+enter","command":"workbench.action.terminal.sendSequence","when":"terminalFocus"}]}"#;
        fixture.put(&path, inherited);
        // CC terminalSetup.tsx:297 invokes keybindings.find, rather than
        // requiring Array.isArray; ordinary prototype and receiver semantics.
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        assert!(output.starts_with("Found existing VSCode terminal Shift+Enter key binding."));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), inherited);
        for content in [
            r#"{"__proto__":[],"find":null}"#,
            r#"{"__proto__":[],"length":1}"#,
        ] {
            fixture.put(&path, content);
            let error = install_bindings_for_vscode_terminal(
                VSCodeFamilyEditor::VSCode,
                crate::utils::theme::ThemeName::Dark,
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Failed to install VSCode terminal Shift+Enter key binding"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
        fixture.put(&path, r#"{"__proto__":[]}"#);
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::VSCode,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        assert!(output.starts_with("Installed VSCode terminal Shift+Enter key binding"));
        assert_eq!(fixture.backups(&path).len(), 4);
    }

    #[tokio::test]
    async fn terminal_setup_remote_installer_matches_official_no_file_effects() {
        let fixture = InstallerFixture::new();
        let _askpass = EnvVarGuard::set(
            "VSCODE_GIT_ASKPASS_MAIN",
            "/remote/.cursor-server/askpass.sh",
        );
        let output = install_bindings_for_vscode_terminal(
            VSCodeFamilyEditor::Cursor,
            crate::utils::theme::ThemeName::Dark,
        )
        .await
        .unwrap();
        // CC terminalSetup.tsx:236-251 returns before mkdir/backup. Snippet
        // indentation and final newline are source text, not JSON.stringify.
        assert!(output.starts_with("Cannot install keybindings from a remote Cursor session.\n\n"));
        assert!(output.ends_with("    \"when\": \"terminalFocus\"\n  }\n]\n"));
        assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 0);
    }
    // Imported-effect doubles only. Source algorithms above run unchanged and
    // no test can invoke defaults, PlistBuddy or killall while this is active.
    pub(super) mod apple_installer {
        use super::super::*;
        use crate::utils::apple_terminal_backup::RestoreResult;
        use crate::utils::exec_file_no_throw::ExecFileOutput;
        use serde_json::{Value, json};
        use std::{cell::RefCell, collections::VecDeque};
        struct State {
            scenario: Value,
            codes: VecDeque<i32>,
            read_codes: VecDeque<i32>,
            profiles: VecDeque<String>,
            events: Vec<Value>,
            logs: Vec<Value>,
        }
        thread_local! {static STATE: RefCell<Option<State>> = const {RefCell::new(None)};}
        pub(in super::super) fn exec(program: &str, args: &[&str]) -> Option<ExecFileOutput> {
            STATE.with(|cell| {
                let mut cell = cell.borrow_mut();
                let state = cell.as_mut()?;
                state.events.push(json!(["exec", program, args]));
                let (code, stdout) = if args.first() == Some(&"read") {
                    (
                        state.read_codes.pop_front().unwrap_or(0),
                        state.profiles.pop_front().unwrap_or_default(),
                    )
                } else {
                    (state.codes.pop_front().unwrap_or(0), String::new())
                };
                Some(ExecFileOutput {
                    code,
                    stdout,
                    stderr: String::new(),
                    error: None,
                })
            })
        }
        pub(in super::super) fn get_terminal_plist_path() -> PathBuf {
            if STATE.with(|cell| cell.borrow().is_some()) {
                PathBuf::from("/oracle/Terminal.plist")
            } else {
                crate::utils::apple_terminal_backup::get_terminal_plist_path()
            }
        }
        pub(in super::super) fn log(error: &crate::utils::log::LogError) {
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    state
                        .logs
                        .push(json!(format!("{}: {}", error.name, error.message)));
                }
            });
        }
        pub(in super::super) async fn backup_terminal_preferences() -> Option<String> {
            let result = STATE.with(|cell| {
                let mut cell = cell.borrow_mut();
                let state = cell.as_mut()?;
                state.events.push(json!(["backup"]));
                Some((state.scenario["backup"] != false).then(|| "/oracle/backup".to_string()))
            });
            if let Some(result) = result {
                result
            } else {
                crate::utils::apple_terminal_backup::backup_terminal_preferences().await
            }
        }
        pub(in super::super) fn mark_terminal_setup_complete() -> anyhow::Result<()> {
            if STATE.with(|cell| {
                let mut cell = cell.borrow_mut();
                if let Some(state) = cell.as_mut() {
                    state.events.push(json!(["complete"]));
                    true
                } else {
                    false
                }
            }) {
                Ok(())
            } else {
                crate::utils::apple_terminal_backup::mark_terminal_setup_complete()
            }
        }
        pub(in super::super) async fn check_and_restore_terminal_backup()
        -> anyhow::Result<RestoreResult> {
            let result = STATE.with(|cell| {
                let mut cell = cell.borrow_mut();
                let state = cell.as_mut()?;
                state.events.push(json!(["restore"]));
                Some(match state.scenario["restore"]["status"].as_str() {
                    Some("restored") => RestoreResult::Restored,
                    Some("failed") => RestoreResult::Failed {
                        backup_path: state.scenario["restore"]["backupPath"]
                            .as_str()
                            .unwrap()
                            .into(),
                    },
                    _ => RestoreResult::NoBackup,
                })
            });
            if let Some(result) = result {
                Ok(result)
            } else {
                crate::utils::apple_terminal_backup::check_and_restore_terminal_backup().await
            }
        }
        struct Guard(u8);
        impl Drop for Guard {
            fn drop(&mut self) {
                STATE.with(|cell| *cell.borrow_mut() = None);
                chalk::set_stdout_level(self.0);
            }
        }
        #[tokio::test]
        async fn terminal_setup_apple_installer_matches_official_bun_argv_effects_and_errors() {
            let _guard = Guard(chalk::stdout_level());
            chalk::set_stdout_level(0);
            let rows:Vec<Value>=serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/oracles/terminal-setup-0913/apple/installer-oracle.json"))).unwrap();
            for row in rows {
                let input = &row["input"];
                let integers = |key: &str| {
                    input[key]
                        .as_array()
                        .map(|values| values.iter().map(|v| v.as_i64().unwrap() as i32).collect())
                        .unwrap_or_default()
                };
                STATE.with(|cell| {
                    *cell.borrow_mut() = Some(State {
                        scenario: input.clone(),
                        codes: integers("codes"),
                        read_codes: integers("readCodes"),
                        profiles: input["profiles"]
                            .as_array()
                            .map(|values| {
                                values
                                    .iter()
                                    .map(|v| v.as_str().unwrap().to_owned())
                                    .collect()
                            })
                            .unwrap_or_default(),
                        events: Vec::new(),
                        logs: Vec::new(),
                    })
                });
                let result =
                    match enable_option_as_meta_for_terminal(crate::utils::theme::ThemeName::Dark)
                        .await
                    {
                        Ok(value) => json!(value),
                        Err(error) => json!({"thrown":error.to_string()}),
                    };
                // CC terminalSetup.tsx:340-499, actual Bun function bodies:
                // two profile reads, Add/Set fallback, partial success, recovery
                // outcome and even the second Chalk dim argument are observable.
                assert_eq!(result, row["result"], "{} result", row["name"]);
                STATE.with(|cell| {
                    let cell = cell.borrow();
                    let state = cell.as_ref().unwrap();
                    assert_eq!(json!(state.events), row["events"], "{} events", row["name"]);
                    assert_eq!(json!(state.logs), row["logs"], "{} logs", row["name"]);
                });
            }
        }
    }
}
