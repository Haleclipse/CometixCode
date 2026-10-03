//! Hook configuration snapshot manager.
//! Maps to: CC `utils/hooks/hooksConfigSnapshot.ts`.
//!
//! This module owns the startup/session snapshot of settings-backed shell hook
//! configuration. It intentionally keeps plugin/session registered hooks out of
//! the snapshot, matching Claude Code: plugin hooks are assembled separately in
//! `utils/hooks.ts#getMatchingHooks`, while this snapshot filters only settings
//! sources according to managed policy and plugin-only customization policy.

use crate::services::hooks::HooksConfig;
use crate::utils::settings::SettingsJson;
use std::sync::{LazyLock, Mutex};

static HOOKS_CONFIG_SNAPSHOT: LazyLock<Mutex<Option<HooksConfig>>> =
    LazyLock::new(|| Mutex::new(None));

fn hooks_config_from_settings(settings: Option<&SettingsJson>) -> HooksConfig {
    settings
        .and_then(|settings| settings.hooks.as_ref())
        .and_then(|hooks| serde_json::from_value::<HooksConfig>(hooks.clone()).ok())
        .unwrap_or_default()
}

/// Maps to: CC `hooksConfigSnapshot.ts#getHooksFromAllowedSources()`.
///
/// `plugin_only_hooks_restricted` is the Rust caller's already-evaluated form
/// of CC `isRestrictedToPluginOnly('hooks')`. Keeping this value as an input
/// preserves source-aware filtering without coupling this service module to UI
/// or settings-loader global state.
pub fn get_hooks_from_allowed_sources(
    merged_settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
    plugin_only_hooks_restricted: bool,
) -> HooksConfig {
    // If managed settings disables all hooks, return empty.
    if policy_settings.is_some_and(|policy| policy.disable_all_hooks == Some(true)) {
        // Maps to CC executeHooksOutsideREPL skip log (snapshot-time equivalent).
        crate::utils::debug::log_for_debugging(
            "Skipping hooks due to 'disableAllHooks' managed setting",
        );
        return HooksConfig::new();
    }

    // If managed settings allows only managed hooks, use policy hooks.
    if policy_settings.is_some_and(|policy| policy.allow_managed_hooks_only == Some(true)) {
        return hooks_config_from_settings(policy_settings);
    }

    // strictPluginOnlyCustomization blocks user/project/local hooks, but plugin
    // hooks are assembled separately by the registered-hooks path.
    if plugin_only_hooks_restricted {
        return hooks_config_from_settings(policy_settings);
    }

    // Non-managed disableAllHooks cannot disable managed hooks, so this becomes
    // managed-only rather than all-off.
    if merged_settings.disable_all_hooks == Some(true) {
        return hooks_config_from_settings(policy_settings);
    }

    hooks_config_from_settings(Some(merged_settings))
}

/// Maps to: CC `hooksConfigSnapshot.ts:62-76` `shouldAllowManagedHooksOnly()`.
///
/// SOLE owner. `services/hooks/security.rs` carried a byte-for-byte second copy
/// of this function and of [`should_disable_all_hooks_including_managed`] until
/// 2026-08-30, with no production caller — every reader already came here, via
/// [`load_hooks_config_from_settings_sources`] and
/// `services/hooks/mod.rs#should_disable_all_hooks_including_managed_from_settings`.
/// The copies still agreed when they were removed; the reason to remove them
/// anyway is that these two are ADMIN controls, and an admin control with two
/// implementations is one bad merge away from an enterprise policy that holds
/// on one code path and not the other.
///
/// True when the policy sets `allowManagedHooksOnly`, or when a NON-managed
/// source sets `disableAllHooks` — user settings cannot disable managed hooks,
/// so they collapse to managed-only instead of off.
pub fn should_allow_managed_hooks_only(
    merged_settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
) -> bool {
    if policy_settings.is_some_and(|policy| policy.allow_managed_hooks_only == Some(true)) {
        return true;
    }
    merged_settings.disable_all_hooks == Some(true)
        && !policy_settings.is_some_and(|policy| policy.disable_all_hooks == Some(true))
}

/// Maps to: CC `hooksConfigSnapshot.ts:83-88`
/// `shouldDisableAllHooksIncludingManaged()`.
///
/// SOLE owner — see [`should_allow_managed_hooks_only`] for the duplicate this
/// replaced. Only POLICY settings can fully disable hooks; `disableAllHooks`
/// from any other source becomes managed-only.
pub fn should_disable_all_hooks_including_managed(policy_settings: Option<&SettingsJson>) -> bool {
    policy_settings.is_some_and(|policy| policy.disable_all_hooks == Some(true))
}

/// Maps to: CC `captureHooksConfigSnapshot()`.
pub fn capture_hooks_config_snapshot(
    merged_settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
    plugin_only_hooks_restricted: bool,
) {
    let snapshot = get_hooks_from_allowed_sources(
        merged_settings,
        policy_settings,
        plugin_only_hooks_restricted,
    );
    *HOOKS_CONFIG_SNAPSHOT
        .lock()
        .expect("hooks config snapshot poisoned") = Some(snapshot);
}

/// Maps to: CC `updateHooksConfigSnapshot()`.
///
/// CC also resets the settings cache before recapturing. Cometix callers pass
/// the already-reloaded settings snapshot into this service function, so no
/// settings-loader side effect is performed here.
pub fn update_hooks_config_snapshot(
    merged_settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
    plugin_only_hooks_restricted: bool,
) {
    capture_hooks_config_snapshot(
        merged_settings,
        policy_settings,
        plugin_only_hooks_restricted,
    );
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
pub fn get_hooks_config_from_snapshot_or_capture(
    merged_settings: &SettingsJson,
    policy_settings: Option<&SettingsJson>,
    plugin_only_hooks_restricted: bool,
) -> HooksConfig {
    if let Some(snapshot) = get_hooks_config_from_snapshot() {
        return snapshot;
    }
    capture_hooks_config_snapshot(
        merged_settings,
        policy_settings,
        plugin_only_hooks_restricted,
    );
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
    pub allow_managed_hooks_only: bool,
    pub disable_all_hooks: bool,
    pub merged_settings: SettingsJson,
    pub policy_settings: Option<SettingsJson>,
}

/// Load and filter settings-backed hooks using the official source-aware gates.
///
/// Maps to: CC `getHooksConfigFromSnapshot()` fallback through
/// `captureHooksConfigSnapshot()` and `getHooksFromAllowedSources()`.
pub fn load_hooks_config_from_settings_sources() -> LoadedHooksConfig {
    let loaded = crate::utils::settings::get_settings_with_errors();
    let policy_settings = loaded.policy_settings.as_ref();
    let plugin_only_hooks_restricted =
        crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only_with_policy(
            "hooks",
            policy_settings.and_then(|settings| settings.strict_plugin_only_customization.as_ref()),
        );
    let allow_managed_hooks_only =
        should_allow_managed_hooks_only(&loaded.settings, policy_settings);
    let disable_all_hooks = should_disable_all_hooks_including_managed(policy_settings);
    let config = get_hooks_config_from_snapshot_or_capture(
        &loaded.settings,
        policy_settings,
        plugin_only_hooks_restricted,
    )
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
        allow_managed_hooks_only,
        disable_all_hooks,
        merged_settings: loaded.settings,
        policy_settings: loaded.policy_settings,
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
    fn policy_disable_all_returns_empty_like_official_managed_gate() {
        let _guard = TEST_LOCK.lock().unwrap();
        let merged = settings_with_hook("echo user");
        let policy = SettingsJson {
            disable_all_hooks: Some(true),
            hooks: Some(serde_json::json!({
                "PreToolUse": [{"hooks": [{"type": "command", "command": "echo managed"}]}]
            })),
            ..SettingsJson::default()
        };

        assert!(get_hooks_from_allowed_sources(&merged, Some(&policy), false).is_empty());
        assert!(should_disable_all_hooks_including_managed(Some(&policy)));
    }

    #[test]
    fn allow_managed_hooks_only_uses_policy_hooks() {
        let _guard = TEST_LOCK.lock().unwrap();
        let merged = settings_with_hook("echo user");
        let policy = SettingsJson {
            allow_managed_hooks_only: Some(true),
            hooks: Some(serde_json::json!({
                "PreToolUse": [{"hooks": [{"type": "command", "command": "echo managed"}]}]
            })),
            ..SettingsJson::default()
        };

        let config = get_hooks_from_allowed_sources(&merged, Some(&policy), false);
        assert_eq!(pre_tool_command(&config), Some("echo managed"));
        assert!(should_allow_managed_hooks_only(&merged, Some(&policy)));
    }

    #[test]
    fn non_managed_disable_all_keeps_only_managed_hooks() {
        let _guard = TEST_LOCK.lock().unwrap();
        let mut merged = settings_with_hook("echo user");
        merged.disable_all_hooks = Some(true);
        let policy = settings_with_hook("echo managed");

        let config = get_hooks_from_allowed_sources(&merged, Some(&policy), false);
        assert_eq!(pre_tool_command(&config), Some("echo managed"));
        assert!(should_allow_managed_hooks_only(&merged, Some(&policy)));
    }

    #[test]
    fn plugin_only_customization_filters_settings_to_policy_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        let merged = settings_with_hook("echo user");
        let policy = settings_with_hook("echo managed");

        let config = get_hooks_from_allowed_sources(&merged, Some(&policy), true);
        assert_eq!(pre_tool_command(&config), Some("echo managed"));
    }

    /// The two rows the deleted `security.rs` copies were the only tests for,
    /// kept with the surviving owner. Both are the NO-POLICY column, which the
    /// snapshot tests above never exercise:
    ///
    /// - `shouldDisableAllHooksIncludingManaged()` reads
    ///   `getSettingsForSource('policySettings')?.disableAllHooks === true`
    ///   (CC `hooksConfigSnapshot.ts:84-87`), and `?.` on an absent policy is
    ///   `undefined !== true` — false, not "unknown";
    /// - `shouldAllowManagedHooksOnly()` is TRUE for a user-level
    ///   `disableAllHooks` with no policy at all (`:69-74`): non-managed
    ///   settings cannot disable managed hooks, so they collapse to
    ///   managed-only. That row is the whole reason the two predicates are not
    ///   negations of each other.
    #[test]
    fn managed_predicates_hold_with_no_policy_settings_at_all() {
        let _guard = TEST_LOCK.lock().unwrap();
        assert!(!should_disable_all_hooks_including_managed(None));

        let user_disabled = SettingsJson {
            disable_all_hooks: Some(true),
            ..SettingsJson::default()
        };
        assert!(should_allow_managed_hooks_only(&user_disabled, None));
        assert!(
            !should_disable_all_hooks_including_managed(None),
            "a user-level disableAllHooks is not an admin off switch"
        );
    }

    #[test]
    fn snapshot_capture_update_and_reset_follow_official_lifecycle() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_hooks_config_snapshot();
        let merged = settings_with_hook("echo first");
        let updated = settings_with_hook("echo second");

        let captured = get_hooks_config_from_snapshot_or_capture(&merged, None, false);
        assert_eq!(pre_tool_command(&captured), Some("echo first"));

        let still_first = get_hooks_config_from_snapshot_or_capture(&updated, None, false);
        assert_eq!(pre_tool_command(&still_first), Some("echo first"));

        update_hooks_config_snapshot(&updated, None, false);
        let second = get_hooks_config_from_snapshot().expect("snapshot");
        assert_eq!(pre_tool_command(&second), Some("echo second"));

        reset_hooks_config_snapshot();
        assert!(get_hooks_config_from_snapshot().is_none());
    }
}
