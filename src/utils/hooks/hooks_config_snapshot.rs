//! Hook configuration snapshot manager.
//! Maps to: CC `utils/hooks/hooksConfigSnapshot.ts`.
//!
//! This module owns the startup/session snapshot of settings-backed shell hook
//! configuration. It intentionally keeps plugin/session registered hooks out of
//! the snapshot, matching Claude Code: plugin hooks are assembled separately in
//! `utils/hooks.ts#getMatchingHooks`, while this snapshot filters only settings
//! sources.

use crate::services::hooks::HooksConfig;
use crate::utils::settings::SettingsJson;
use std::sync::{LazyLock, Mutex};

static HOOKS_CONFIG_SNAPSHOT: LazyLock<Mutex<Option<HooksConfig>>> =
    LazyLock::new(|| Mutex::new(None));

fn hooks_config_from_settings(settings: &SettingsJson) -> HooksConfig {
    settings
        .hooks
        .as_ref()
        .and_then(|hooks| serde_json::from_value::<HooksConfig>(hooks.clone()).ok())
        .unwrap_or_default()
}

/// Maps to: CC `hooksConfigSnapshot.ts#getHooksFromAllowedSources()`.
///
/// @nyan: CC also consults managed settings (`disableAllHooks`,
/// `allowManagedHooksOnly`, `strictPluginOnlyCustomization`). nyan has no
/// managed source, so CC's "a non-managed `disableAllHooks` keeps only managed
/// hooks" leaves nothing: `disableAllHooks` turns every settings hook off.
pub fn get_hooks_from_allowed_sources(merged_settings: &SettingsJson) -> HooksConfig {
    if merged_settings.disable_all_hooks == Some(true) {
        return HooksConfig::new();
    }
    hooks_config_from_settings(merged_settings)
}

/// Maps to: CC `captureHooksConfigSnapshot()`.
pub fn capture_hooks_config_snapshot(merged_settings: &SettingsJson) {
    let snapshot = get_hooks_from_allowed_sources(merged_settings);
    *HOOKS_CONFIG_SNAPSHOT
        .lock()
        .expect("hooks config snapshot poisoned") = Some(snapshot);
}

/// Maps to: CC `updateHooksConfigSnapshot()`.
///
/// CC also resets the settings cache before recapturing. Nyan callers pass
/// the already-reloaded settings snapshot into this service function, so no
/// settings-loader side effect is performed here.
pub fn update_hooks_config_snapshot(merged_settings: &SettingsJson) {
    capture_hooks_config_snapshot(merged_settings);
}

/// Maps to: CC `getHooksConfigFromSnapshot()`.
pub fn get_hooks_config_from_snapshot() -> Option<HooksConfig> {
    HOOKS_CONFIG_SNAPSHOT
        .lock()
        .expect("hooks config snapshot poisoned")
        .clone()
}

/// Testable fallback form of CC `getHooksConfigFromSnapshot()`, which captures
/// from settings if no snapshot exists.
pub fn get_hooks_config_from_snapshot_or_capture(merged_settings: &SettingsJson) -> HooksConfig {
    if let Some(snapshot) = get_hooks_config_from_snapshot() {
        return snapshot;
    }
    capture_hooks_config_snapshot(merged_settings);
    get_hooks_config_from_snapshot().unwrap_or_default()
}

/// Maps to call sites using CC `getHooksConfigFromSnapshot()` plus
/// `shouldAllowManagedHooksOnly()`.
#[derive(Clone, Debug, Default)]
pub struct LoadedHooksConfig {
    /// The execution-facing table: settings entries folded into the
    /// `RegisteredHookMatcher` shape the registered (SDK/plugin) channel
    /// shares (CC's `getHooksConfig` union by typing).
    pub config: crate::schemas::hooks::RegisteredHooks,
    /// The merged `disableAllHooks`: settings, plugin and session hooks are
    /// off; SDK callback hooks still run. CC's `shouldAllowManagedHooksOnly()`
    /// with no managed settings.
    pub disable_all_hooks: bool,
    pub merged_settings: SettingsJson,
}

/// Load and filter settings-backed hooks.
///
/// Maps to: CC `getHooksConfigFromSnapshot()` fallback through
/// `captureHooksConfigSnapshot()` and `getHooksFromAllowedSources()`.
pub fn load_hooks_config_from_settings_sources() -> LoadedHooksConfig {
    let loaded = crate::utils::settings::get_settings_with_errors();
    let disable_all_hooks = loaded.settings.disable_all_hooks == Some(true);
    let config = get_hooks_config_from_snapshot_or_capture(&loaded.settings)
        .into_iter()
        .map(|(event, entries)| {
            (
                event,
                entries
                    .iter()
                    .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry)
                    .collect(),
            )
        })
        .collect();
    LoadedHooksConfig {
        config,
        disable_all_hooks,
        merged_settings: loaded.settings,
    }
}

/// Maps to: CC `resetHooksConfigSnapshot()`.
pub fn reset_hooks_config_snapshot() {
    *HOOKS_CONFIG_SNAPSHOT
        .lock()
        .expect("hooks config snapshot poisoned") = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TestStateLock;

    static TEST_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

    fn settings_with_hook(command: &str) -> SettingsJson {
        SettingsJson {
            hooks: Some(serde_json::json!({
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": command}]
                }]
            })),
            ..SettingsJson::default()
        }
    }

    fn pre_tool_command(config: &HooksConfig) -> Option<&str> {
        config
            .get("PreToolUse")?
            .first()?
            .hooks
            .first()
            .map(|hook| hook.command.as_str())
    }

    #[test]
    fn disable_all_hooks_turns_every_settings_hook_off() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut merged = settings_with_hook("echo user");
        assert_eq!(
            pre_tool_command(&get_hooks_from_allowed_sources(&merged)),
            Some("echo user")
        );
        merged.disable_all_hooks = Some(true);
        assert!(get_hooks_from_allowed_sources(&merged).is_empty());
    }

    #[test]
    fn snapshot_capture_update_and_reset_follow_official_lifecycle() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_hooks_config_snapshot();
        let merged = settings_with_hook("echo first");
        let updated = settings_with_hook("echo second");

        let captured = get_hooks_config_from_snapshot_or_capture(&merged);
        assert_eq!(pre_tool_command(&captured), Some("echo first"));

        let still_first = get_hooks_config_from_snapshot_or_capture(&updated);
        assert_eq!(pre_tool_command(&still_first), Some("echo first"));

        update_hooks_config_snapshot(&updated);
        let second = get_hooks_config_from_snapshot().expect("snapshot");
        assert_eq!(pre_tool_command(&second), Some("echo second"));

        reset_hooks_config_snapshot();
        assert!(get_hooks_config_from_snapshot().is_none());
    }
}
