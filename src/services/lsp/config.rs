//! LSP server configuration loading.
//!
//! Maps to: CC `services/lsp/config.ts` `getAllLspServers()`.
//!
//! Official Claude Code loads LSP server definitions from enabled plugins only;
//! user/project settings do not define LSP servers directly. CometixCode keeps
//! that boundary by reading the cache-only plugin loader and resolving plugin
//! LSP declarations through `utils/plugins/lsp_plugin_integration.rs`.

use crate::services::lsp::types::ScopedLspServerConfig;
use indexmap::IndexMap;

/// Maps to the object returned by CC `getAllLspServers()`.
///
/// `IndexMap`, not `BTreeMap`: CC returns a `Record` and the manager iterates
/// it with `Object.entries(...)`, so key order is INSERTION order and it
/// decides which server wins an extension (`LSPServerManager.ts:89`, `:201`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllLspServers {
    pub servers: IndexMap<String, ScopedLspServerConfig>,
}

/// Maps to: CC `services/lsp/config.ts` `getAllLspServers()`.
///
/// Note the CC body wraps everything in one try/catch that logs and returns
/// `{servers: {}}` (`:67-78`), so this loader is infallible on both sides; the
/// `Result` in `LSPServerManager::initialize`'s loader type is the declared
/// `@throws` contract of `initialize()` itself (`:70`), not a new failure mode
/// invented here.
pub fn get_all_lsp_servers() -> AllLspServers {
    let plugins = crate::utils::plugins::plugin_loader::load_all_plugins_cache_only_from_sync();
    let mut servers: IndexMap<String, ScopedLspServerConfig> = IndexMap::new();

    for plugin in plugins.enabled {
        let errors = std::sync::Mutex::new(Vec::new());
        if let Some(scoped_servers) =
            crate::utils::plugins::lsp_plugin_integration::get_plugin_lsp_servers_readonly(
                &plugin, &errors,
            )
        {
            // Maps to CC `Object.assign` merge order: plugins keep their load
            // order, and a later plugin overwrites a colliding scoped name in
            // place (which is also what `IndexMap::extend` does).
            servers.extend(scoped_servers);
        }
    }

    AllLspServers { servers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn get_all_lsp_servers_reads_enabled_inline_plugin_lsp_configs() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-lsp-config-plugin-{}",
            uuid::Uuid::new_v4()
        ));
        let config_home =
            std::env::temp_dir().join(format!("cometix-lsp-config-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".claude-plugin")).expect("plugin dir");
        std::fs::create_dir_all(&config_home).expect("config home");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let previous_inline = crate::bootstrap::state::get_inline_plugins();
        crate::bootstrap::state::set_inline_plugins(vec![root.clone()]);
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "lspServers": {
                "rust": {
                  "command":"rust-analyzer",
                  "extensionToLanguage":{".rs":"rust"}
                }
              }
            }"#,
        )
        .expect("manifest");

        let loaded = get_all_lsp_servers();
        let rust = loaded
            .servers
            .get("plugin:toolbox:rust")
            .expect("plugin-scoped rust LSP");
        assert_eq!(rust.config.command, "rust-analyzer");
        assert_eq!(rust.config.transport.as_deref(), Some("stdio"));
        assert_eq!(rust.scope.as_deref(), Some("dynamic"));
        assert_eq!(rust.source.as_deref(), Some("toolbox"));

        crate::bootstrap::state::set_inline_plugins(previous_inline);
        let _ = std::fs::remove_dir_all(config_home);
        let _ = std::fs::remove_dir_all(root);
    }
}
