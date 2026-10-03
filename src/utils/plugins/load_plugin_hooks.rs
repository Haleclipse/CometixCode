//! Maps to: CC `utils/plugins/loadPluginHooks.ts`.
//!
//! Plugin discovery remains cache-only/read-only; matchers are atomically
//! registered in CC's process-level `registeredHooks` store.

use crate::services::hooks::HooksConfig;
use crate::types::plugin::LoadedPlugin;
use crate::utils::plugins::plugin_loader::load_all_plugins_cache_only_from_sync;
use std::sync::{Arc, LazyLock, Mutex};

/// Rust memoization cell for CC's memoized `loadPluginHooks`. This is only a
/// cache bit; the authoritative `registeredHooks` value lives in bootstrap
/// state exactly like CC.
static PLUGIN_HOOKS_LOADED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

struct HotReloadRegistration {
    _subscription: crate::utils::settings::change_detector::SettingsChangeUnsubscribe,
}

/// The source installs one process-wide settings listener after startup. Keep
/// the unsubscribe handle alive for that process lifetime, while the snapshot
/// itself remains independently mutable from the callback.
static HOT_RELOAD_REGISTRATION: LazyLock<Mutex<Option<HotReloadRegistration>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps to: CC `convertPluginHooksToMatchers(plugin)`.
pub fn convert_plugin_hooks_to_matchers(plugin: &LoadedPlugin) -> Result<HooksConfig, String> {
    let Some(value) = plugin.hooks_config.as_ref() else {
        return Ok(HooksConfig::new());
    };
    let mut config = serde_json::from_value::<HooksConfig>(command_hook_projection(value))
        .map_err(|error| {
            format!(
                "Failed to parse hooks for plugin {} ({}): {error}",
                plugin.name,
                plugin.path.display()
            )
        })?;
    let plugin_root = plugin.path.display().to_string();
    for entries in config.values_mut() {
        for entry in entries {
            entry.plugin_root = Some(plugin_root.clone());
            entry.plugin_name = Some(plugin.name.clone());
            entry.plugin_id = Some(plugin.source.clone());
        }
    }
    Ok(config)
}

fn command_hook_projection(value: &serde_json::Value) -> serde_json::Value {
    let mut projected = value.clone();
    let Some(events) = projected.as_object_mut() else {
        return projected;
    };
    for matchers in events.values_mut() {
        let Some(matchers) = matchers.as_array_mut() else {
            continue;
        };
        for matcher in matchers.iter_mut() {
            if let Some(hooks) = matcher
                .get_mut("hooks")
                .and_then(serde_json::Value::as_array_mut)
            {
                hooks.retain(|hook| {
                    hook.get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some()
                        && hook
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(|kind| kind == "command")
                });
            }
        }
        matchers.retain(|matcher| {
            matcher
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|hooks| !hooks.is_empty())
        });
    }
    projected
}

/// Maps to: CC memoized `loadPluginHooks()`.
///
/// The clear/register operation is atomic: existing registered hooks remain in
/// place until the complete replacement registry has been assembled.
pub fn load_plugin_hooks() -> Result<(), String> {
    let mut loaded = PLUGIN_HOOKS_LOADED
        .lock()
        .map_err(|_| "plugin hook memoization lock poisoned".to_string())?;
    if *loaded {
        return Ok(());
    }

    let plugins = load_all_plugins_cache_only_from_sync();
    let mut registered = HooksConfig::new();
    let mut errors = Vec::new();
    for plugin in &plugins.enabled {
        match convert_plugin_hooks_to_matchers(plugin) {
            Ok(config) => merge_hooks_config(&mut registered, config),
            Err(error) => errors.push(error),
        }
    }

    // Fold the settings-shaped plugin matchers into the execution-facing
    // `RegisteredHooks` table the bootstrap store now carries.
    let registered: crate::schemas::hooks::RegisteredHooks = registered
        .iter()
        .map(|(event, entries)| {
            (
                event.clone(),
                entries
                    .iter()
                    .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry)
                    .collect(),
            )
        })
        .collect();
    {
        // CC loadPluginHooks.ts:145-146: one synchronous clear/register pair.
        // I/O and conversion above complete before acquiring the native turn.
        let _turn = crate::state::store::enter_store_turn_segment();
        crate::bootstrap::state::clear_registered_plugin_hooks();
        #[cfg(test)]
        tests::observe_full_swap_for_test();
        crate::bootstrap::state::register_hook_callbacks(registered);
    }
    *loaded = true;

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn merge_hooks_config(target: &mut HooksConfig, source: HooksConfig) {
    for (event, mut entries) in source {
        target.entry(event).or_default().append(&mut entries);
    }
}

/// Maps to: CC `clearPluginHookCache()` (invalidate hook memoization only).
/// Orphaned-version exclusions remain frozen until the explicit plugin reload
/// owner calls `clear_plugin_cache_exclusions()` separately.
pub fn clear_plugin_hook_cache() {
    if let Ok(mut loaded) = PLUGIN_HOOKS_LOADED.lock() {
        *loaded = false;
    }
}

/// Maps to: CC `setupPluginHookHotReload()`
/// (`utils/plugins/loadPluginHooks.ts:255-287`). The source listens only to
/// `policySettings`; a change reloads when one of the settings consumed by
/// `loadAllPluginsCacheOnly` changes. Other settings notifications are ignored
/// so ordinary user/project edits do not trigger a plugin refresh.
pub fn setup_plugin_hook_hot_reload() {
    if crate::utils::env_utils::is_bare_mode() {
        return;
    }
    let mut registration = HOT_RELOAD_REGISTRATION.lock().unwrap();
    if registration.is_some() {
        return;
    }

    let snapshot = Arc::new(Mutex::new(plugin_affecting_settings_snapshot()));
    let callback_snapshot = snapshot.clone();
    let subscription =
        crate::utils::settings::change_detector::subscribe(Arc::new(move |source| {
            if source != crate::utils::settings::constants::SettingSource::Policy {
                return;
            }
            let next = plugin_affecting_settings_snapshot();
            let changed = {
                let mut current = callback_snapshot.lock().unwrap();
                if *current == next {
                    false
                } else {
                    *current = next;
                    true
                }
            };
            if !changed {
                crate::utils::debug::log_for_debugging(
                    "Plugin hooks: skipping reload, plugin-affecting settings unchanged",
                );
                return;
            }

            crate::utils::debug::log_for_debugging(
                "Plugin hooks: reloading due to plugin-affecting settings change",
            );
            crate::utils::plugins::plugin_loader::clear_plugin_cache(Some(
                "loadPluginHooks: plugin-affecting settings changed",
            ));
            clear_plugin_hook_cache();

            let Some(runtime) = crate::utils::process_runtime::runtime_handle_for_detached_work()
            else {
                crate::utils::debug::log_for_debugging(
                    "Plugin hooks: reload skipped because the process runtime is unavailable",
                );
                return;
            };
            runtime.spawn(async {
                let result = tokio::task::spawn_blocking(load_plugin_hooks).await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
                            "Plugin hooks hot reload failed: {error}"
                        )))
                    }
                    Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(
                        format!("Plugin hooks hot reload worker failed: {error}"),
                    )),
                }
            });
        }));
    *registration = Some(HotReloadRegistration {
        _subscription: subscription,
    });
}

/// Maps to: CC `getPluginAffectingSettingsSnapshot()`
/// (`utils/plugins/loadPluginHooks.ts:233-248`). Only the top-level record
/// keys are sorted, matching the source's `sortKeys` helper; nested values keep
/// their existing JSON representation.
fn plugin_affecting_settings_snapshot() -> String {
    use crate::utils::settings::constants::SettingSource;

    let merged = crate::utils::settings::get_initial_settings();
    let policy = crate::utils::settings::get_settings_for_source(SettingSource::Policy);
    plugin_affecting_settings_snapshot_from(&merged, policy.as_ref())
}

fn plugin_affecting_settings_snapshot_from(
    merged: &crate::utils::settings::SettingsJson,
    policy: Option<&crate::utils::settings::SettingsJson>,
) -> String {
    use serde_json::{Map, Value};

    fn sorted_record(value: Option<&Value>) -> Value {
        let Some(object) = value.and_then(Value::as_object) else {
            return Value::Object(Map::new());
        };
        let mut keys = object.keys().collect::<Vec<_>>();
        keys.sort();
        let mut sorted = Map::new();
        for key in keys {
            sorted.insert(key.clone(), object[key].clone());
        }
        Value::Object(sorted)
    }

    serde_json::json!({
        "enabledPlugins": sorted_record(merged.enabled_plugins.as_ref()),
        "extraKnownMarketplaces": sorted_record(merged.extra_known_marketplaces.as_ref()),
        "strictKnownMarketplaces": policy.and_then(|settings| settings.strict_known_marketplaces.clone()).unwrap_or_default(),
        "blockedMarketplaces": policy.and_then(|settings| settings.blocked_marketplaces.clone()).unwrap_or_default(),
    })
    .to_string()
}

/// Maps to: CC `utils/plugins/loadPluginHooks.ts:179-211#pruneRemovedPluginHooks`.
pub async fn prune_removed_plugin_hooks() -> anyhow::Result<()> {
    if crate::bootstrap::state::get_registered_hooks().is_none() {
        return Ok(());
    }
    let loaded = super::plugin_loader::load_all_plugins_cache_only().await?;
    let enabled: std::collections::HashSet<_> = loaded
        .enabled
        .iter()
        .map(|p| p.path.to_string_lossy().into_owned())
        .collect();
    // CC loadPluginHooks.ts:188-210: re-read and swap are one synchronous
    // continuation, so a concurrent full refresh cannot interleave with it.
    let _turn = crate::state::store::enter_store_turn_segment();
    let Some(current) = crate::bootstrap::state::get_registered_hooks() else {
        return Ok(());
    };
    #[cfg(test)]
    tests::observe_prune_for_test();
    let survivors = current
        .into_iter()
        .filter_map(|(event, matchers)| {
            let kept: Vec<_> = matchers
                .into_iter()
                .filter(|m| {
                    m.plugin_root
                        .as_ref()
                        .is_some_and(|root| enabled.contains(root))
                })
                .collect();
            (!kept.is_empty()).then_some((event, kept))
        })
        .collect();
    crate::bootstrap::state::clear_registered_plugin_hooks();
    crate::bootstrap::state::register_hook_callbacks(survivors);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::plugin::LoadedPlugin;
    use crate::utils::plugins::schemas::PluginManifest;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;
    use std::path::PathBuf;

    thread_local! {
        // Test-only observation of the real source clear/register boundary.
        static FULL_SWAP_OBSERVER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            std::cell::RefCell::new(None);
        static PRUNE_OBSERVER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            std::cell::RefCell::new(None);
    }

    pub(super) fn observe_full_swap_for_test() {
        let observer = FULL_SWAP_OBSERVER.with(|slot| slot.borrow_mut().take());
        if let Some(observer) = observer {
            observer();
        }
    }

    pub(super) fn observe_prune_for_test() {
        let observer = PRUNE_OBSERVER.with(|slot| slot.borrow_mut().take());
        if let Some(observer) = observer {
            observer();
        }
    }

    /// Maps to CC loadPluginHooks.ts:145-146 and :188-210: neither a reader nor
    /// the competing prune continuation can observe/interleave a half swap.
    #[test]
    fn full_swap_and_concurrent_prune_match_official_atomic_hook_registration() {
        use crate::bootstrap::state::{get_registered_hooks, register_hook_callbacks};
        use crate::schemas::hooks::{HookCallback, RegisteredHook, RegisteredHookMatcher};
        use std::sync::{Arc, mpsc};
        use std::time::Duration;

        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        struct TestDir(PathBuf);
        impl TestDir {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = TestDir(
            std::env::temp_dir().join(format!("cometix-hook-swap-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.path().join("config"));
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "0");
        let previous_inline = crate::bootstrap::state::get_inline_plugins();
        let previous_hooks = get_registered_hooks();
        struct Restore(Vec<PathBuf>, Option<crate::schemas::hooks::RegisteredHooks>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(self.0.clone());
                match self.1.take() {
                    Some(hooks) => crate::bootstrap::state::replace_registered_hooks(hooks),
                    None => crate::bootstrap::state::clear_registered_hooks(),
                }
                super::super::plugin_loader::clear_plugin_cache(None);
                clear_plugin_hook_cache();
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _restore = Restore(previous_inline, previous_hooks);
        let plugin = root.path().join("plugin");
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            json!({
                "name":"atomic-hook-fixture",
                "hooks":{"Stop":[{"hooks":[{"type":"command","command":"never-executed"}]}]}
            })
            .to_string(),
        )
        .unwrap();
        crate::bootstrap::state::set_inline_plugins(vec![plugin]);
        super::super::plugin_loader::clear_plugin_cache(None);
        clear_plugin_hook_cache();
        crate::utils::settings::settings_cache::reset_settings_cache();
        let loaded = crate::utils::process_runtime::block_on_from_sync(async {
            super::super::plugin_loader::load_all_plugins().await
        })
        .unwrap()
        .unwrap();
        assert_eq!(loaded.enabled.len(), 1);
        crate::bootstrap::state::clear_registered_hooks();
        register_hook_callbacks(std::collections::HashMap::from([(
            "Stop".into(),
            vec![RegisteredHookMatcher {
                hooks: vec![RegisteredHook::Callback(HookCallback {
                    callback: Arc::new(|_, _| Box::pin(async { json!({}) })),
                    timeout: None,
                })],
                ..Default::default()
            }],
        )]));

        let (paused_tx, paused_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (full_tx, full_rx) = mpsc::channel();
        let full = std::thread::spawn(move || {
            FULL_SWAP_OBSERVER.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    paused_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                }))
            });
            full_tx.send(load_plugin_hooks()).unwrap();
        });
        paused_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // This is the actual full loader paused after clear, not a second
        // hand-written swap implementation. Race both a reader and real prune.
        let (started_tx, started_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        let reader_started = started_tx.clone();
        let reader_completed = completed_tx.clone();
        let reader = std::thread::spawn(move || {
            reader_started.send(()).unwrap();
            reader_completed
                .send(("reader", get_registered_hooks().unwrap()))
                .unwrap();
        });
        let prune = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            crate::utils::process_runtime::block_on_from_sync(prune_removed_plugin_hooks())
                .unwrap()
                .unwrap();
            completed_tx
                .send(("prune", get_registered_hooks().unwrap()))
                .unwrap();
        });
        for _ in 0..2 {
            started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        let early = completed_rx.recv_timeout(Duration::from_millis(100));
        // Always release the production worker before asserting, including
        // under a regression, so the test does not strand the process turn.
        release_tx.send(()).unwrap();
        full_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        full.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "a consumer observed the half-swap: {early:?}"
        );
        for _ in 0..2 {
            let (consumer, hooks) = completed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let stop = &hooks["Stop"];
            assert_eq!(
                stop.len(),
                2,
                "{consumer}: callbacks and latest plugin exactly once"
            );
            assert_eq!(stop.iter().filter(|m| m.plugin_root.is_none()).count(), 1);
            assert_eq!(
                stop.iter()
                    .filter(|m| m.plugin_name.as_deref() == Some("atomic-hook-fixture"))
                    .count(),
                1
            );
        }
        reader.join().unwrap();
        prune.join().unwrap();

        // Reverse the race: pause the real prune after its current-table read.
        // A full load must not replace the table until this prune commits.
        let mut old = get_registered_hooks().unwrap();
        old.get_mut("Stop")
            .unwrap()
            .iter_mut()
            .filter(|matcher| matcher.plugin_root.is_some())
            .for_each(|matcher| matcher.plugin_name = Some("old-version".into()));
        crate::bootstrap::state::replace_registered_hooks(old);
        clear_plugin_hook_cache();
        let (paused_tx, paused_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (pruned_tx, pruned_rx) = mpsc::channel();
        let prune = std::thread::spawn(move || {
            PRUNE_OBSERVER.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    paused_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                }))
            });
            let result =
                crate::utils::process_runtime::block_on_from_sync(prune_removed_plugin_hooks())
                    .unwrap();
            pruned_tx.send(result).unwrap();
        });
        paused_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (full_tx, full_rx) = mpsc::channel();
        let full = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            full_tx.send(load_plugin_hooks()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let early = full_rx.recv_timeout(Duration::from_millis(100));
        release_tx.send(()).unwrap();
        pruned_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        prune.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "full refresh interleaved with prune's read/commit: {early:?}"
        );
        full_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        full.join().unwrap();
        let final_hooks = get_registered_hooks().unwrap();
        assert_eq!(final_hooks["Stop"].len(), 2);
        assert_eq!(
            final_hooks["Stop"]
                .iter()
                .filter(|matcher| matcher.plugin_name.as_deref() == Some("atomic-hook-fixture"))
                .count(),
            1
        );
        assert_eq!(
            final_hooks["Stop"]
                .iter()
                .filter(|matcher| matcher.plugin_root.is_none())
                .count(),
            1
        );
    }

    #[test]
    fn convert_plugin_hooks_preserves_plugin_matcher_context() {
        let plugin = LoadedPlugin {
            name: "formatter".to_string(),
            source: "formatter@inline".to_string(),
            path: PathBuf::from("/plugins/formatter"),
            manifest: PluginManifest {
                name: "formatter".to_string(),
                ..PluginManifest::default()
            },
            agents_path: None,
            agents_paths: Vec::new(),
            enabled: true,
            is_builtin: false,
            hooks_config: Some(json!({
                "SessionStart": [{
                    "matcher": "resume",
                    "hooks": [
                        {"type": "command", "command": "echo plugin"},
                        {"type": "prompt", "prompt": "unsupported in command projection"}
                    ]
                }]
            })),
            mcp_servers: Default::default(),
            lsp_servers: Default::default(),
            ..Default::default()
        };

        let config = convert_plugin_hooks_to_matchers(&plugin).unwrap();
        let matcher = &config["SessionStart"][0];
        assert_eq!(matcher.plugin_root.as_deref(), Some("/plugins/formatter"));
        assert_eq!(matcher.plugin_name.as_deref(), Some("formatter"));
        assert_eq!(matcher.plugin_id.as_deref(), Some("formatter@inline"));
        assert_eq!(matcher.hooks.len(), 1);
    }

    #[test]
    fn plugin_hook_hot_reload_snapshot_sorts_source_record_keys() {
        let merged = crate::utils::settings::SettingsJson {
            enabled_plugins: Some(json!({"z@market": true, "a@market": false})),
            extra_known_marketplaces: Some(json!({"z": "./z", "a": "./a"})),
            ..Default::default()
        };
        let policy = crate::utils::settings::SettingsJson {
            strict_known_marketplaces: Some(vec![json!("official")]),
            blocked_marketplaces: Some(vec![json!("blocked")]),
            ..Default::default()
        };
        assert_eq!(
            plugin_affecting_settings_snapshot_from(&merged, Some(&policy)),
            r#"{"enabledPlugins":{"a@market":false,"z@market":true},"extraKnownMarketplaces":{"a":"./a","z":"./z"},"strictKnownMarketplaces":["official"],"blockedMarketplaces":["blocked"]}"#
        );
    }
}
