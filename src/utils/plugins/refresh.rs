//! Maps to: CC `utils/plugins/refresh.ts`.
//! Active-component refresh is separate from settings intent and installation.

use crate::commands::Command;
use crate::state::store::{AppStore, UpdateDecision};
use crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult;
use crate::types::plugin::PluginError;
use crate::utils::debug::log_for_debugging;
use std::sync::Arc;

/// Maps to: CC `refresh.ts#RefreshActivePluginsResult`.
#[derive(Clone, Debug, Default)]
pub struct RefreshActivePluginsResult {
    pub enabled_count: usize,
    pub disabled_count: usize,
    pub command_count: usize,
    pub agent_count: usize,
    pub hook_count: usize,
    pub mcp_count: usize,
    pub lsp_count: usize,
    pub error_count: usize,
    pub agent_definitions: Arc<AgentDefinitionsResult>,
    pub plugin_commands: Arc<Vec<Command>>,
}

/// Maps to: CC `refresh.ts#refreshActivePlugins`.
/// The existing synchronous command/agent/LSP readers run on blocking workers;
/// their results rejoin at the source Promise.all boundaries, off the UI frame.
pub async fn refresh_active_plugins(
    store: &AppStore,
) -> anyhow::Result<RefreshActivePluginsResult> {
    log_for_debugging("refreshActivePlugins: clearing all plugin caches");
    super::cache_utils::clear_all_caches();
    super::orphaned_plugin_filter::clear_plugin_cache_exclusions();

    // Full discovery must warm cache-only readers before either is started.
    let loaded = super::plugin_loader::load_all_plugins().await?;
    let (commands, agents) = tokio::join!(
        super::load_plugin_commands::get_plugin_commands(),
        tokio::task::spawn_blocking(|| {
            crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides(
                &crate::bootstrap::state::get_original_cwd(),
            )
        }),
    );
    let plugin_commands = commands?;
    let agent_definitions = Arc::new(agents?);

    let mcp_counts = futures::future::join_all(loaded.enabled.iter().map(|plugin| async {
        if let Some(servers) = plugin.mcp_servers.snapshot() {
            return servers.as_object().map_or(0, |servers| servers.len());
        }
        let servers = super::mcp_plugin_integration::load_plugin_mcp_servers(
            plugin,
            loaded.errors.as_mutex(),
        )
        .await;
        let count = servers.as_ref().map_or(0, |servers| servers.len());
        if let Some(servers) = servers {
            plugin.mcp_servers.set(Some(
                serde_json::to_value(servers).expect("MCP configuration serializes"),
            ));
        }
        count
    }));
    let lsp_counts = futures::future::join_all(loaded.enabled.iter().map(|plugin| {
        let plugin = plugin.clone();
        let errors = loaded.errors.clone();
        async move {
            if let Some(servers) = plugin.lsp_servers.snapshot() {
                return Ok(servers.as_object().map_or(0, |servers| servers.len()));
            }
            tokio::task::spawn_blocking(move || {
                let servers = super::lsp_plugin_integration::load_plugin_lsp_servers_readonly(
                    &plugin,
                    errors.as_mutex(),
                );
                let count = servers.as_ref().map_or(0, |servers| servers.len());
                if let Some(servers) = servers {
                    plugin.lsp_servers.set(Some(
                        serde_json::to_value(servers).expect("LSP configuration serializes"),
                    ));
                }
                count
            })
            .await
        }
    }));
    let (mcp_counts, lsp_counts) = tokio::join!(mcp_counts, lsp_counts);
    let mcp_count = mcp_counts.into_iter().sum();
    let lsp_count = lsp_counts
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum();

    store.set_state(|previous| {
        let mut next = (**previous).clone();
        let plugins = Arc::make_mut(&mut next.plugins);
        plugins.enabled = loaded.enabled.clone();
        plugins.disabled = loaded.disabled.clone();
        plugins.commands = plugin_commands.clone();
        plugins.errors = merge_plugin_errors(&previous.plugins.errors, &loaded.errors.snapshot());
        plugins.needs_refresh = false;
        next.agent_definitions = agent_definitions.clone();
        Arc::make_mut(&mut next.mcp).plugin_reconnect_key += 1;
        UpdateDecision::Replace {
            next: Arc::new(next),
            result: (),
        }
    });

    // Unconditional: removing the last LSP plugin also invalidates LSP config.
    crate::services::lsp::manager::reinitialize_lsp_server_manager();
    let hook_load =
        tokio::task::spawn_blocking(super::load_plugin_hooks::load_plugin_hooks).await?;
    let hook_load_failed = hook_load.is_err();
    if let Err(error) = hook_load {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.clone()));
        log_for_debugging(&format!(
            "refreshActivePlugins: loadPluginHooks failed: {error}"
        ));
    }

    let hook_count = loaded
        .enabled
        .iter()
        .map(|plugin| {
            plugin
                .hooks_config
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flat_map(|events| events.values())
                .filter_map(serde_json::Value::as_array)
                .flatten()
                .filter_map(|matcher| matcher.get("hooks").and_then(serde_json::Value::as_array))
                .map(Vec::len)
                .sum::<usize>()
        })
        .sum();
    log_for_debugging(&format!(
        "refreshActivePlugins: {} enabled, {} commands, {} agents, {hook_count} hooks, {mcp_count} MCP, {lsp_count} LSP",
        loaded.enabled.len(),
        plugin_commands.len(),
        agent_definitions.all_agents.len(),
    ));
    Ok(RefreshActivePluginsResult {
        enabled_count: loaded.enabled.len(),
        disabled_count: loaded.disabled.len(),
        command_count: plugin_commands.len(),
        agent_count: agent_definitions.all_agents.len(),
        hook_count,
        mcp_count,
        lsp_count,
        error_count: loaded.errors.len() + usize::from(hook_load_failed),
        agent_definitions,
        plugin_commands,
    })
}

/// Maps to: CC `refresh.ts#mergePluginErrors`.
fn merge_plugin_errors(existing: &[PluginError], fresh: &[PluginError]) -> Vec<PluginError> {
    let fresh_keys = fresh
        .iter()
        .map(error_key)
        .collect::<std::collections::HashSet<_>>();
    existing
        .iter()
        .filter(|error| error.source() == "lsp-manager" || error.source().starts_with("plugin:"))
        .filter(|error| !fresh_keys.contains(&error_key(error)))
        .chain(fresh)
        .cloned()
        .collect()
}

/// Maps to: CC `refresh.ts#errorKey`.
fn error_key(error: &PluginError) -> String {
    if let PluginError::GenericError { source, error, .. } = error {
        format!("generic-error:{source}:{error}")
    } else {
        let value = serde_json::to_value(error).expect("PluginError serializes");
        format!("{}:{}", value["type"].as_str().unwrap(), error.source())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    fn error(source: &str, text: &str) -> PluginError {
        PluginError::GenericError {
            source: source.into(),
            plugin: None,
            error: text.into(),
        }
    }

    #[test]
    fn refresh_error_merge_matches_actual_bun_oracle_and_preserves_fresh_duplicates() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-contract-review-0915/refresh-oracle.json"
        )).unwrap();
        let existing = vec![
            error("lsp-manager", "preserved"),
            error("plugin:p1", "old-distinct"),
            error("plugin:p1", "mcp-error"),
            error("other", "discarded"),
        ];
        let fresh = vec![
            error("load", "initial"),
            error("plugin:p2", "mcp-error"),
            error("plugin:p1", "lsp-error"),
            error("plugin:p1", "mcp-error"),
            error("plugin:p2", "lsp-error"),
        ];
        assert_eq!(
            json!(
                merge_plugin_errors(&existing, &fresh)
                    .iter()
                    .map(error_key)
                    .collect::<Vec<_>>()
            ),
            oracle["errorKeys"]
        );
        let duplicate = error("plugin:p1", "same");
        assert_eq!(
            merge_plugin_errors(
                std::slice::from_ref(&duplicate),
                &[duplicate.clone(), duplicate.clone()]
            ),
            vec![duplicate.clone(), duplicate]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_cold_load_publishes_components_warms_slots_and_replaces_removed_plugin() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        struct TestDir(std::path::PathBuf);
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
        let root =
            TestDir(std::env::temp_dir().join(format!("cometix-refresh-{}", uuid::Uuid::new_v4())));
        let config = root.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "0");
        let previous_inline = crate::bootstrap::state::get_inline_plugins();
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_hooks = crate::bootstrap::state::get_registered_hooks();
        struct Restore(
            Vec<std::path::PathBuf>,
            std::path::PathBuf,
            Option<crate::schemas::hooks::RegisteredHooks>,
        );
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(self.0.clone());
                crate::bootstrap::state::set_original_cwd(self.1.clone());
                crate::bootstrap::state::clear_registered_hooks();
                if let Some(hooks) = self.2.take() {
                    crate::bootstrap::state::register_hook_callbacks(hooks);
                }
                super::super::plugin_loader::clear_plugin_cache(None);
                super::super::load_plugin_hooks::clear_plugin_hook_cache();
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _restore = Restore(previous_inline, previous_cwd, previous_hooks);
        crate::bootstrap::state::set_original_cwd(root.path());
        crate::bootstrap::state::clear_registered_hooks();
        crate::utils::settings::settings_cache::reset_settings_cache();
        let plugin = root.path().join("plugin");
        for dir in [".claude-plugin", "commands", "agents"] {
            std::fs::create_dir_all(plugin.join(dir)).unwrap();
        }
        std::fs::write(plugin.join(".claude-plugin/plugin.json"), json!({
            "name":"reload-fixture",
            "hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf unused"}]}]},
            "mcpServers":{"fixture":{"command":"not-executed"}},
            "lspServers":{"fixture":{"command":"not-executed","extensionToLanguage":{".fixture":"fixture"}}}
        }).to_string()).unwrap();
        std::fs::write(
            plugin.join("commands/check.md"),
            "---\ndescription: Reload test command\n---\nNever executed",
        )
        .unwrap();
        std::fs::write(
            plugin.join("agents/checker.md"),
            "---\nname: checker\ndescription: Reload test agent\n---\nNever executed",
        )
        .unwrap();
        crate::bootstrap::state::set_inline_plugins(vec![plugin]);
        let mut initial = crate::state::app_state_store::AppState::default();
        Arc::make_mut(&mut initial.plugins).needs_refresh = true;
        Arc::make_mut(&mut initial.plugins).errors = vec![
            error("lsp-manager", "preserved"),
            error("old-loader", "discarded"),
        ];
        Arc::make_mut(&mut initial.mcp).plugin_reconnect_key = 7;
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_clone = observed.clone();
        let store = AppStore::new(
            initial,
            Some(Arc::new(move |next, _| {
                observed_clone.lock().unwrap().push((
                    next.mcp.plugin_reconnect_key,
                    next.plugins.needs_refresh,
                    next.plugins
                        .enabled
                        .iter()
                        .all(|p| p.mcp_servers.is_some() && p.lsp_servers.is_some()),
                ));
            })),
        );

        let result = refresh_active_plugins(&store).await.unwrap();
        assert!(Arc::ptr_eq(
            &result.plugin_commands,
            &store.get().plugins.commands
        ));
        assert!(Arc::ptr_eq(
            &result.agent_definitions,
            &store.get().agent_definitions
        ));
        assert_eq!(
            (
                result.enabled_count,
                result.disabled_count,
                result.command_count,
                result.hook_count,
                result.mcp_count,
                result.lsp_count,
                result.error_count
            ),
            (1, 0, 1, 1, 1, 1, 0)
        );
        assert!(
            result
                .agent_definitions
                .all_agents
                .iter()
                .any(|agent| agent.agent_type == "reload-fixture:checker")
        );
        assert_eq!(*observed.lock().unwrap(), vec![(8, false, true)]);
        assert_eq!(
            store.get().plugins.errors,
            vec![error("lsp-manager", "preserved")]
        );
        let cached = super::super::plugin_loader::load_all_plugins_cache_only()
            .await
            .unwrap();
        let cached_plugin = &cached.enabled[0];
        assert_eq!(
            cached_plugin.mcp_servers.snapshot().unwrap()["fixture"]["command"],
            "not-executed"
        );
        assert_eq!(
            cached_plugin.lsp_servers.snapshot().unwrap()["fixture"]["command"],
            "not-executed"
        );
        assert!(
            crate::bootstrap::state::get_registered_hooks().unwrap()["Stop"]
                .iter()
                .any(|h| h.plugin_name.as_deref() == Some("reload-fixture"))
        );

        // Explicit reload sees a new disk selection; old cached objects stay old.
        let empty = root.path().join("empty-plugin");
        std::fs::create_dir_all(empty.join(".claude-plugin")).unwrap();
        std::fs::write(
            empty.join(".claude-plugin/plugin.json"),
            r#"{"name":"empty-fixture"}"#,
        )
        .unwrap();
        crate::bootstrap::state::set_inline_plugins(vec![empty]);
        let removed = refresh_active_plugins(&store).await.unwrap();
        assert_eq!(
            (
                removed.command_count,
                removed.hook_count,
                removed.mcp_count,
                removed.lsp_count
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(store.get().mcp.plugin_reconnect_key, 9);
        assert_eq!(store.get().plugins.enabled[0].name, "empty-fixture");
        assert!(cached_plugin.mcp_servers.is_some());
        assert!(
            super::super::plugin_loader::load_all_plugins_cache_only()
                .await
                .unwrap()
                .enabled[0]
                .mcp_servers
                .is_none()
        );
        assert!(
            crate::bootstrap::state::get_registered_hooks()
                .unwrap_or_default()
                .values()
                .flatten()
                .all(|hook| hook.plugin_name.as_deref() != Some("reload-fixture"))
        );
    }
}
