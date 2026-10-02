//! Maps to: CC `hooks/useManagePlugins.ts`.
use crate::context::notifications::{
    Notification, NotificationColor, NotificationPriority, NotificationsWriter,
};
use crate::state::store::{AppStore, UpdateDecision};
use crate::utils::process_env::JsTruthy;
use crate::types::plugin::PluginError;
use iocraft::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// Maps to: CC `useManagePlugins.ts#useManagePlugins`.
pub fn use_manage_plugins(hooks: &mut Hooks<'_, '_>, enabled: bool) {
    let store = crate::state::app_state::use_app_state_store(hooks);
    let needs_refresh = crate::state::app_state::use_app_state(hooks, |s| s.plugins.needs_refresh);
    let notifications = crate::context::notifications::use_notifications(hooks);
    let mount_notifications = notifications.clone();
    hooks.use_effect(
        move || {
            if !enabled {
                return;
            }
            let store = store.clone();
            let notifications = mount_notifications.clone();
            crate::utils::process_runtime::runtime_handle_for_detached_work()
                .expect("plugin mount requires process runtime")
                .spawn(async move {
                    let mut metrics = initial_plugin_load(store, notifications).await;
                    let names = metrics.as_object_mut().unwrap().remove("ant_enabled_names");
                    metrics["has_custom_plugin_cache_dir"] = json!(
                        crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_CACHE_DIR")
                            .truthy()
                            .is_some()
                    );
                    let mut event = metrics.clone();
                    if let Some(names) = names.filter(|v| !v.is_null()) {
                        event["enabled_names"] = names;
                    }
                    crate::services::analytics::log_event("tengu_plugins_loaded", event);
                    // diagLogs has no Rust output sink; preserve base fields in the existing debug sink.
                    crate::utils::debug::log_for_debugging(&format!(
                        "tengu_plugins_loaded: {metrics}"
                    ));
                });
        },
        enabled,
    );
    hooks.use_effect(
        move || {
            if !enabled || !needs_refresh {
                return;
            }
            let mut notifications = notifications.clone();
            notifications.add_notification(
                Notification::text(
                    "plugin-reload-pending",
                    "Plugins changed. Run /reload-plugins to activate.",
                    NotificationPriority::Low,
                )
                .with_color(NotificationColor::Suggestion),
            );
        },
        (enabled, needs_refresh),
    );
}

/// Maps to: CC `useManagePlugins.ts#initialPluginLoad`, retained async callback.
async fn initial_plugin_load(store: AppStore, mut notifications: NotificationsWriter) -> Value {
    let result: anyhow::Result<Value> = async {
            let loaded = crate::utils::plugins::plugin_loader::load_all_plugins().await?;
            crate::utils::plugins::plugin_blocklist::detect_and_uninstall_delisted_plugins().await;
            if !crate::utils::plugins::plugin_flagging::get_flagged_plugins().is_empty() {
                notifications.add_notification(
                    Notification::text(
                        "plugin-delisted-flagged",
                        "Plugins flagged. Check /plugins",
                        NotificationPriority::High,
                    )
                    .with_color(NotificationColor::Warning),
                );
            }
            let commands = match crate::utils::plugins::load_plugin_commands::get_plugin_commands().await {
                Ok(commands) => commands,
                Err(error) => {
                    loaded.errors.push(PluginError::GenericError {
                        source: "plugin-commands".into(), plugin: None,
                        error: format!("Failed to load plugin commands: {error}"),
                    });
                    Arc::new(Vec::new())
                }
            };
            let agents = crate::utils::plugins::load_plugin_agents::load_plugin_agents();
            if let Err(error) = crate::utils::plugins::load_plugin_hooks::load_plugin_hooks() {
                loaded.errors.push(PluginError::GenericError {
                    source: "plugin-hooks".into(),
                    plugin: None,
                    error: format!("Failed to load plugin hooks: {error}"),
                });
            }
            // Maps to: useManagePlugins.ts:119-127 Promise.all. Each plugin
            // starts independently; result counts retain the enabled-list order.
            let error_sink = loaded.errors.as_mutex();
            let mcp_counts =
                futures::future::join_all(loaded.enabled.iter().map(|plugin| async move {
                    if let Some(servers) = plugin.mcp_servers.snapshot() {
                        return servers.as_object().map_or(0, |v| v.len());
                    }
                    let servers =
                        crate::utils::plugins::mcp_plugin_integration::load_plugin_mcp_servers(
                            plugin, error_sink,
                        )
                        .await;
                    let count = servers.as_ref().map_or(0, |servers| servers.len());
                    if let Some(servers) = servers {
                        plugin.mcp_servers.set(Some(
                            serde_json::to_value(servers).expect("MCP configuration serializes"),
                        ));
                    }
                    count
                }))
                .await;
            let mcp_count = mcp_counts.into_iter().sum::<usize>();
            // Source second Promise.all starts each LSP load independently.
            // Existing synchronous filesystem adapter runs off the async worker;
            // shared errors are pushed at their source sites, never buffered.
            let lsp_counts = futures::future::join_all(loaded.enabled.iter().map(|plugin| {
                let plugin = plugin.clone();
                let errors = loaded.errors.clone();
                async move {
                    if let Some(servers) = plugin.lsp_servers.snapshot() {
                        return servers.as_object().map_or(0, |v| v.len());
                    }
                    tokio::task::spawn_blocking(move || {
                        let servers = crate::utils::plugins::lsp_plugin_integration::load_plugin_lsp_servers_readonly(&plugin, errors.as_mutex());
                        let count = servers.as_ref().map_or(0, |servers| servers.len());
                        if let Some(servers) = servers {
                            plugin.lsp_servers.set(Some(serde_json::to_value(servers).expect("LSP configuration serializes")));
                        }
                        count
                    }).await.expect("plugin LSP loader task")
                }
            })).await;
            let lsp_count = lsp_counts.into_iter().sum::<usize>();
            crate::services::lsp::manager::reinitialize_lsp_server_manager();
            let error_count = loaded.errors.len();
            store.set_state(|previous| {
                let mut next = (**previous).clone();
                let plugins = Arc::make_mut(&mut next.plugins);
                let error_key = |error: &PluginError| {
                    let value = serde_json::to_value(error).expect("PluginError serializes");
                    if let PluginError::GenericError { source, error, .. } = error {
                        format!("generic-error:{source}:{error}")
                    } else {
                        format!("{}:{}", value["type"].as_str().unwrap(), error.source())
                    }
                };
                let loaded_errors = loaded.errors.snapshot();
                let new_keys = loaded_errors
                    .iter()
                    .map(error_key)
                    .collect::<std::collections::HashSet<_>>();
                let mut merged = plugins
                    .errors
                    .iter()
                    .filter(|e| {
                        (e.source() == "lsp-manager" || e.source().starts_with("plugin:"))
                            && !new_keys.contains(&error_key(e))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                merged.extend(loaded_errors);
                plugins.enabled = loaded.enabled.clone();
                plugins.disabled = loaded.disabled.clone();
                plugins.commands = commands.clone();
                plugins.errors = merged;
                UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
            crate::utils::debug::log_for_debugging(&format!(
                "Loaded plugins - Enabled: {}, Disabled: {}, Commands: {}, Agents: {}, Errors: {}",
                loaded.enabled.len(),
                loaded.disabled.len(),
                commands.len(),
                agents.len(),
                error_count
            ));
            let hook_count: usize = loaded
                .enabled
                .iter()
                .filter_map(|p| p.hooks_config.as_ref())
                .filter_map(|v| v.as_object())
                .flat_map(|v| v.values())
                .filter_map(|v| v.as_array())
                .flatten()
                .map(|m| m["hooks"].as_array().map_or(0, |h| h.len()))
                .sum();
            let mut metrics = json!({"enabled_count":loaded.enabled.len(),"disabled_count":loaded.disabled.len(),"inline_count":loaded.enabled.iter().filter(|p|p.source.ends_with("@inline")).count(),"marketplace_count":loaded.enabled.iter().filter(|p|!p.source.ends_with("@inline")).count(),"error_count":error_count,"skill_count":commands.len(),"agent_count":agents.len(),"hook_count":hook_count,"mcp_count":mcp_count,"lsp_count":lsp_count});
            if crate::utils::process_env::var("USER_TYPE").as_deref() == Some("ant")
                && !loaded.enabled.is_empty()
            {
                let mut names = loaded
                    .enabled
                    .iter()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>();
                names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                metrics["ant_enabled_names"] = json!(names.join(","));
            }
            Ok(metrics)
    }.await;
    match result {
        Ok(metrics) => metrics,
        Err(error) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            crate::utils::debug::log_for_debugging(&format!("Error loading plugins: {error}"));
            store.set_state(|previous| {
                let mut next = (**previous).clone();
                let plugins = Arc::make_mut(&mut next.plugins);
                plugins.enabled.clear();
                plugins.disabled.clear();
                plugins.commands = std::sync::Arc::new(Vec::new());
                plugins
                    .errors
                    .retain(|e| e.source() == "lsp-manager" || e.source().starts_with("plugin:"));
                plugins.errors.push(PluginError::GenericError {
                    source: "plugin-system".into(),
                    plugin: None,
                    error: error.to_string(),
                });
                UpdateDecision::Replace {
                    next: Arc::new(next),
                    result: (),
                }
            });
            json!({"enabled_count":0,"disabled_count":0,"inline_count":0,"marketplace_count":0,"error_count":1,"skill_count":0,"agent_count":0,"hook_count":0,"mcp_count":0,"lsp_count":0,"load_failed":true})
        }
    }
}
