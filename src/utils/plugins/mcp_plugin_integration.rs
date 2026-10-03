//! Plugin MCP integration.
//!
//! Maps to: CC `utils/plugins/mcpPluginIntegration.ts`.
//!
//! The canonical async loader includes MCPB materialization. Existing synchronous
//! MCP configuration consumers use an off-frame bridge before resolving variables.

use crate::services::mcp::types::{ConfigScope, ScopedMcpServerConfig};
use crate::types::plugin::LoadedPlugin;
use crate::types::plugin::PluginError;
use crate::utils::config::McpServerConfig;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Maps to: CC `mcpPluginIntegration.ts:589-640#getPluginMcpServers`.
pub async fn get_plugin_mcp_servers(
    plugin: &LoadedPlugin,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<indexmap::IndexMap<String, ScopedMcpServerConfig>> {
    if !plugin.enabled {
        return None;
    }
    let servers = if let Some(cached) = plugin.mcp_servers.snapshot() {
        native_inline_mcp_servers(&cached)
    } else {
        load_plugin_mcp_servers(plugin, errors).await
    }?;

    let mut resolved = indexmap::IndexMap::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(&servers)
        .into_iter()
        .map(|(name, config)| (name.to_owned(), config.clone()))
    {
        let user_config = build_mcp_user_config_readonly(plugin, &name);
        match resolve_plugin_mcp_config_readonly(config, plugin, user_config.as_ref()) {
            Ok((config, missing_env_vars)) => {
                if !missing_env_vars.is_empty() && !plugin.name.is_empty() && !name.is_empty() {
                    errors.lock().unwrap().push(PluginError::McpConfigInvalid {
                        source: format!("plugin:{}", plugin.name),
                        plugin: plugin.name.clone(),
                        server_name: name.clone(),
                        validation_error: format!(
                            "Missing environment variables: {}",
                            missing_env_vars.join(", ")
                        ),
                    });
                }
                resolved.insert(name, config);
            }
            Err(error) => errors.lock().unwrap().push(PluginError::GenericError {
                source: name,
                plugin: Some(plugin.name.clone()),
                error,
            }),
        }
    }

    Some(add_plugin_scope_to_servers(
        resolved,
        &plugin.name,
        &plugin.source,
    ))
}

fn build_mcp_user_config_readonly(
    plugin: &LoadedPlugin,
    server_name: &str,
) -> Option<serde_json::Map<String, Value>> {
    // Maps to CC `mcpPluginIntegration.ts#buildMcpUserConfig`: merge
    // top-level manifest.userConfig values with channel-specific per-server
    // saved config; channel-specific wins on collision.
    let mut top_level = plugin.manifest.user_config.as_ref().map(|_| {
        crate::utils::plugins::plugin_options_storage::load_plugin_options(
            &crate::utils::plugins::plugin_options_storage::get_plugin_storage_id(plugin),
        )
    });
    let channel_specific = plugin_channel_user_config_schema(plugin, server_name).and_then(|_| {
        crate::utils::plugins::mcpb_handler::load_mcp_server_user_config(
            &crate::utils::plugins::plugin_options_storage::get_plugin_storage_id(plugin),
            server_name,
        )
    });

    match (top_level.as_mut(), channel_specific) {
        (None, None) => None,
        (Some(values), None) => Some(values.clone()),
        (None, Some(values)) => Some(values),
        (Some(values), Some(channel_values)) => {
            values.extend(channel_values);
            Some(values.clone())
        }
    }
}

fn plugin_channel_user_config_schema(plugin: &LoadedPlugin, server_name: &str) -> Option<Value> {
    plugin
        .manifest
        .channels
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|channels| {
            channels.iter().find_map(|channel| {
                (channel.get("server").and_then(Value::as_str) == Some(server_name))
                    .then(|| channel.get("userConfig").cloned())
                    .flatten()
            })
        })
}

fn resolve_plugin_mcp_config_readonly(
    mut config: McpServerConfig,
    plugin: &LoadedPlugin,
    user_config: Option<&serde_json::Map<String, Value>>,
) -> Result<(McpServerConfig, Vec<String>), String> {
    // Maps to CC `mcpPluginIntegration.ts#resolvePluginMcpEnvironment` for
    // plugin variables, `${user_config.X}`, and general `${ENV}` expansion.
    let mut missing_env_vars = Vec::new();
    config.command = config
        .command
        .map(|value| {
            resolve_plugin_mcp_string_value(&value, plugin, user_config, &mut missing_env_vars)
        })
        .transpose()?;
    config.args = config
        .args
        .map(|args| {
            args.into_iter()
                .map(|value| {
                    resolve_plugin_mcp_string_value(
                        &value,
                        plugin,
                        user_config,
                        &mut missing_env_vars,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    config.url = config
        .url
        .map(|value| {
            resolve_plugin_mcp_string_value(&value, plugin, user_config, &mut missing_env_vars)
        })
        .transpose()?;
    config.headers_helper = config
        .headers_helper
        .map(|value| {
            resolve_plugin_mcp_string_value(&value, plugin, user_config, &mut missing_env_vars)
        })
        .transpose()?;
    let is_stdio = plugin_mcp_config_is_stdio(&config);
    let existing_env = config.env.take();
    config.env = if is_stdio {
        let mut env = std::collections::HashMap::new();
        // Maps to CC `resolvePluginMcpEnvironment(...)`: stdio plugin MCP
        // servers always receive CLAUDE_PLUGIN_ROOT and CLAUDE_PLUGIN_DATA.
        // DATA is created via getPluginDataDir even when no config string
        // explicitly references it; errors reach the existing per-server catch.
        env.insert(
            "CLAUDE_PLUGIN_ROOT".to_string(),
            crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
                "${CLAUDE_PLUGIN_ROOT}",
                &plugin.path,
                None,
            )?,
        );
        env.insert(
            "CLAUDE_PLUGIN_DATA".to_string(),
            crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
                "${CLAUDE_PLUGIN_DATA}",
                &plugin.path,
                Some(&plugin.source),
            )?,
        );
        if let Some(existing) = existing_env {
            for (key, value) in existing {
                let resolved = if key == "CLAUDE_PLUGIN_ROOT" || key == "CLAUDE_PLUGIN_DATA" {
                    value
                } else {
                    resolve_plugin_mcp_string_value(
                        &value,
                        plugin,
                        user_config,
                        &mut missing_env_vars,
                    )?
                };
                env.insert(key, resolved);
            }
        }
        Some(env)
    } else {
        existing_env
            .map(|env| {
                env.into_iter()
                    .map(|(key, value)| {
                        resolve_plugin_mcp_string_value(
                            &value,
                            plugin,
                            user_config,
                            &mut missing_env_vars,
                        )
                        .map(|value| (key, value))
                    })
                    .collect::<Result<_, _>>()
            })
            .transpose()?
    };
    config.headers = config
        .headers
        .map(|headers| {
            headers
                .into_iter()
                .map(|(key, value)| {
                    resolve_plugin_mcp_string_value(
                        &value,
                        plugin,
                        user_config,
                        &mut missing_env_vars,
                    )
                    .map(|value| (key, value))
                })
                .collect::<Result<_, _>>()
        })
        .transpose()?;
    missing_env_vars.sort();
    missing_env_vars.dedup();
    Ok((config, missing_env_vars))
}

fn plugin_mcp_config_is_stdio(config: &McpServerConfig) -> bool {
    // Maps to CC `resolvePluginMcpEnvironment(...)` switch cases
    // `case undefined` and `case 'stdio'`.
    config
        .server_type
        .as_deref()
        .map(|kind| kind == "stdio")
        .unwrap_or(true)
}

fn resolve_plugin_mcp_string_value(
    value: &str,
    plugin: &LoadedPlugin,
    user_config: Option<&serde_json::Map<String, Value>>,
    missing_env_vars: &mut Vec<String>,
) -> Result<String, String> {
    let rendered = crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
        value,
        &plugin.path,
        Some(&plugin.source),
    )?;
    let rendered = if let Some(user_config) = user_config {
        crate::utils::plugins::plugin_options_storage::substitute_user_config_variables(
            &rendered,
            user_config,
        )?
    } else {
        rendered
    };
    let (expanded, missing) =
        crate::services::mcp::env_expansion::expand_env_vars_in_string(&rendered);
    missing_env_vars.extend(missing);
    Ok(expanded)
}

/// Maps to: CC `mcpPluginIntegration.ts:33-120#loadMcpServersFromMcpb`.
async fn load_mcp_servers_from_mcpb(
    plugin: &LoadedPlugin,
    mcpb_path: &str,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<indexmap::IndexMap<String, McpServerConfig>> {
    use super::mcpb_handler::{McpbFileResult, load_mcpb_file};
    crate::utils::debug::log_for_debugging(&format!("Loading MCP servers from MCPB: {mcpb_path}"));
    let name = plugin.name.clone();
    let progress = move |status: &str| {
        crate::utils::debug::log_for_debugging(&format!("MCPB [{name}]: {status}"));
        Ok(())
    };
    let outcome = load_mcpb_file(
        mcpb_path,
        &plugin.path,
        &plugin.repository,
        Some(&progress),
        None,
        false,
    )
    .await;
    match outcome {
        Ok(McpbFileResult::NeedsConfig(_)) => {
            crate::utils::debug::log_for_debugging(&format!(
                "MCPB {mcpb_path} requires user configuration. User can configure via: /plugin → Manage plugins → {} → Configure",
                plugin.name
            ));
            None
        }
        Ok(McpbFileResult::Loaded(result)) => {
            let server_name = result.manifest["name"]
                .as_str()
                .expect("validated MCPB name")
                .to_owned();
            crate::utils::debug::log_for_debugging(&format!(
                "Loaded MCP server \"{server_name}\" from MCPB (extracted to {})",
                result.extracted_path.display()
            ));
            // Native typed projection of generateMcpConfig's canonical output.
            let config = serde_json::from_value(result.mcp_config)
                .expect("generated MCPB config fits native MCP carrier");
            Some(indexmap::IndexMap::from([(server_name, config)]))
        }
        Err(error) => {
            let message = error.to_string();
            crate::utils::debug::log_for_debugging_with_level(
                &format!("Failed to load MCPB {mcpb_path}: {message}"),
                crate::utils::debug::DebugLogLevel::Error,
            );
            let source = format!("{}@{}", plugin.name, plugin.repository);
            errors.lock().unwrap().push(
                if mcpb_path.starts_with("http")
                    && (message.contains("download") || message.contains("network"))
                {
                    PluginError::McpbDownloadFailed {
                        source,
                        plugin: plugin.name.clone(),
                        url: mcpb_path.into(),
                        reason: message,
                    }
                } else if message.contains("manifest") || message.contains("user configuration") {
                    PluginError::McpbInvalidManifest {
                        source,
                        plugin: plugin.name.clone(),
                        mcpb_path: mcpb_path.into(),
                        validation_error: message,
                    }
                } else {
                    PluginError::McpbExtractFailed {
                        source,
                        plugin: plugin.name.clone(),
                        mcpb_path: mcpb_path.into(),
                        reason: message,
                    }
                },
            );
            None
        }
    }
}

/// Maps to: CC `mcpPluginIntegration.ts:131-208#loadPluginMcpServers`.
pub async fn load_plugin_mcp_servers(
    plugin: &LoadedPlugin,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<indexmap::IndexMap<String, McpServerConfig>> {
    let mut servers = load_mcp_servers_from_file(&plugin.path, ".mcp.json")
        .await
        .unwrap_or_default();
    if let Some(spec) = &plugin.manifest.mcp_servers {
        match spec {
            Value::String(path) => {
                let loaded = if super::mcpb_handler::is_mcpb_source(path) {
                    load_mcp_servers_from_mcpb(plugin, path, errors).await
                } else {
                    load_mcp_servers_from_file(&plugin.path, path).await
                };
                if let Some(loaded) = loaded {
                    servers.extend(loaded);
                }
            }
            Value::Array(specs) => {
                // Promise.all starts every spec together; errors append when each
                // operation finishes, while server collisions merge in source order.
                let results = futures::future::join_all(specs.iter().map(move |spec| async move {
                    if let Some(path) = spec.as_str() {
                        if super::mcpb_handler::is_mcpb_source(path) {
                            return load_mcp_servers_from_mcpb(plugin, path, errors).await;
                        }
                        return load_mcp_servers_from_file(&plugin.path, path).await;
                    }
                    native_inline_mcp_servers(spec)
                }))
                .await;
                for loaded in results.into_iter().flatten() {
                    servers.extend(loaded);
                }
            }
            _ => {
                if let Some(loaded) = native_inline_mcp_servers(spec) {
                    servers.extend(loaded);
                }
            }
        }
    }
    (!servers.is_empty()).then_some(servers)
}

// Native projection of already validated inline configuration. The source does
// not run a second schema gate here; the file branch below owns its own gate.
fn native_inline_mcp_servers(value: &Value) -> Option<indexmap::IndexMap<String, McpServerConfig>> {
    serde_json::from_value(value.clone()).ok()
}

/// Maps to: CC `mcpPluginIntegration.ts:220-268#loadMcpServersFromFile`.
async fn load_mcp_servers_from_file(
    plugin_path: &Path,
    relative_path: &str,
) -> Option<indexmap::IndexMap<String, McpServerConfig>> {
    let file_path = super::plugin_loader::node_path_join(plugin_path, relative_path);
    let content = match crate::utils::fs_operations::get_fs_implementation()
        .read_file(
            &file_path,
            crate::utils::fs_operations::BufferEncoding::Utf8,
        )
        .await
    {
        // Node fs.readFile(..., {encoding:'utf-8'}) replaces malformed UTF-8.
        Ok(content) => content.to_string_lossy(),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "Failed to load MCP servers from {}: {error}",
                        file_path.display()
                    ),
                    crate::utils::debug::DebugLogLevel::Error,
                );
            }
            return None;
        }
    };
    let result: anyhow::Result<indexmap::IndexMap<String, McpServerConfig>> = (|| {
        let parsed = crate::utils::slow_operations::json_parse(&content)?.to_json();
        if parsed.is_null() {
            anyhow::bail!("TypeError: null is not an object (evaluating 'parsed.mcpServers')");
        }
        let data = parsed
            .get("mcpServers")
            .filter(|value| {
                !matches!(value, Value::Null | Value::Bool(false))
                    && value.as_str() != Some("")
                    && value.as_f64() != Some(0.0)
            })
            .unwrap_or(&parsed);
        // Object.entries boxes arrays/strings, whereas Rust's as_object only
        // accepts maps. Keep this representation projection at the source call.
        let entries: Vec<(String, Value)> = match data {
            Value::Object(object) => crate::utils::process_env::ecmascript_object_entries(object)
                .into_iter()
                .map(|(name, config)| (name.to_owned(), config.clone()))
                .collect(),
            Value::Array(array) => array
                .iter()
                .enumerate()
                .map(|(index, config)| (index.to_string(), config.clone()))
                .collect(),
            Value::String(text) => text
                .encode_utf16()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        index.to_string(),
                        Value::String(String::from_utf16_lossy(&[unit])),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut servers = indexmap::IndexMap::new();
        for (name, config) in entries {
            match crate::utils::zod::safe_parse(
                crate::services::mcp::types::mcp_server_config_schema(),
                &config,
            ) {
                Ok(config) => {
                    // Source assigns into {}; its inherited __proto__ setter
                    // changes the prototype, without creating an own server.
                    // Validation still runs before that assignment.
                    if name != "__proto__" {
                        servers.insert(name, serde_json::from_value(config)?);
                    }
                }
                Err(error) => crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "Invalid MCP server config for {name} in {}: {}",
                        file_path.display(),
                        error.message()
                    ),
                    crate::utils::debug::DebugLogLevel::Error,
                ),
            }
        }
        Ok(servers)
    })();
    match result {
        Ok(servers) => Some(servers),
        Err(error) => {
            crate::utils::debug::log_for_debugging_with_level(
                &format!(
                    "Failed to load MCP servers from {}: {error}",
                    file_path.display()
                ),
                crate::utils::debug::DebugLogLevel::Error,
            );
            None
        }
    }
}

/// Maps to CC `mcpPluginIntegration.ts#addPluginScopeToServers`.
pub fn add_plugin_scope_to_servers(
    servers: indexmap::IndexMap<String, McpServerConfig>,
    plugin_name: &str,
    plugin_source: &str,
) -> indexmap::IndexMap<String, ScopedMcpServerConfig> {
    crate::utils::process_env::ecmascript_object_entries(&servers)
        .into_iter()
        .map(|(name, config)| {
            let scoped_name = format!("plugin:{plugin_name}:{name}");
            let mut scoped = ScopedMcpServerConfig::from_config(ConfigScope::Dynamic, config);
            scoped.plugin_source = Some(plugin_source.to_string());
            (scoped_name, scoped)
        })
        .collect()
}

/// Maps to: CC `mcpPluginIntegration.ts:366-428#extractMcpServersFromPlugins`.
pub async fn extract_mcp_servers_from_plugins(
    plugins: &[LoadedPlugin],
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> indexmap::IndexMap<String, ScopedMcpServerConfig> {
    let scoped_results = futures::future::join_all(plugins.iter().map(|plugin| async move {
        if !plugin.enabled {
            return None;
        }
        let servers = load_plugin_mcp_servers(plugin, errors).await?;
        let mut resolved = indexmap::IndexMap::new();
        for (name, config) in crate::utils::process_env::ecmascript_object_entries(&servers)
            .into_iter()
            .map(|(name, config)| (name.to_owned(), config.clone()))
        {
            let user_config = build_mcp_user_config_readonly(plugin, &name);
            match resolve_plugin_mcp_config_readonly(config, plugin, user_config.as_ref()) {
                Ok((config, missing_env_vars)) => {
                    if !missing_env_vars.is_empty() && !plugin.name.is_empty() && !name.is_empty() {
                        errors.lock().unwrap().push(PluginError::McpConfigInvalid {
                            source: format!("plugin:{}", plugin.name),
                            plugin: plugin.name.clone(),
                            server_name: name.clone(),
                            validation_error: format!(
                                "Missing environment variables: {}",
                                missing_env_vars.join(", ")
                            ),
                        });
                    }
                    resolved.insert(name, config);
                }
                Err(error) => errors.lock().unwrap().push(PluginError::GenericError {
                    source: name,
                    plugin: Some(plugin.name.clone()),
                    error,
                }),
            }
        }

        // Cache unresolved values, so subsequent activation resolves fresh settings.
        plugin.mcp_servers.set(Some(
            serde_json::to_value(&servers).expect("MCP configuration serializes"),
        ));
        crate::utils::debug::log_for_debugging(&format!(
            "Loaded {} MCP servers from plugin {}",
            servers.len(),
            plugin.name
        ));
        Some(add_plugin_scope_to_servers(
            resolved,
            &plugin.name,
            &plugin.source,
        ))
    }))
    .await;
    let mut all_servers = indexmap::IndexMap::new();
    for servers in scoped_results.into_iter().flatten() {
        all_servers.extend(servers);
    }
    all_servers
}

/// Maps to: CC mcpPluginIntegration.ts:272-276#UnconfiguredChannel.
#[derive(Clone, Debug)]
pub struct UnconfiguredChannel {
    pub server: String,
    pub display_name: String,
    pub config_schema: Value,
}
/// Maps to: CC mcpPluginIntegration.ts:290-318#getUnconfiguredChannels.
pub fn get_unconfigured_channels(
    plugin: &crate::types::plugin::LoadedPlugin,
) -> Vec<UnconfiguredChannel> {
    let mut result = Vec::new();
    for channel in plugin
        .manifest
        .channels
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(schema) = channel
            .get("userConfig")
            .filter(|s| s.as_object().is_some_and(|m| !m.is_empty()))
        else {
            continue;
        };
        let server = channel.get("server").and_then(Value::as_str).unwrap_or("");
        let saved = super::mcpb_handler::load_mcp_server_user_config(&plugin.repository, server)
            .unwrap_or_default();
        if !super::mcpb_handler::validate_user_config(&saved, schema).valid {
            result.push(UnconfiguredChannel {
                server: server.into(),
                display_name: channel
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(server)
                    .into(),
                config_schema: schema.clone(),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::plugin::get_plugin_error_message;
    use crate::utils::plugins::plugin_loader::create_plugin_from_path_for_test as create_plugin_from_path;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::io::Write;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cometix-plugin-mcp-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        let mut file = fs::File::create(path).expect("file");
        file.write_all(content.as_bytes()).expect("write");
    }

    /// Repoints the settings root at a scratch config home for one test.
    ///
    /// `load_plugin_options` reads `pluginConfigs` through
    /// `get_initial_settings`, which is cached process-wide
    /// (`settings_cache.rs:10-12`). Moving `CLAUDE_CONFIG_DIR` alone leaves the
    /// previous test's snapshot in place, so each of these tests reads a
    /// sibling's `pluginConfigs` and reports its own options as missing.
    struct ConfigHomeGuard(Option<EnvVarGuard>);

    impl ConfigHomeGuard {
        fn pin(config_home: &Path) -> Self {
            let guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", config_home);
            crate::utils::settings::settings_cache::reset_settings_cache();
            Self(Some(guard))
        }
    }

    impl Drop for ConfigHomeGuard {
        fn drop(&mut self) {
            drop(self.0.take());
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }

    #[test]
    fn plugin_mcp_servers_merge_default_file_and_manifest_with_scoped_names() {
        let root = temp_dir("merge");
        write_file(
            &root.join(".mcp.json"),
            r#"{"mcpServers":{"default":{"type":"stdio","command":"default-mcp"}}}"#,
        );
        write_file(
            &root.join("servers.json"),
            r#"{"fromFile":{"type":"http","url":"https://example.test/mcp"}}"#,
        );
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "mcpServers":[
                "./servers.json",
                {"inline":{"type":"stdio","command":"${CLAUDE_PLUGIN_ROOT}/inline-mcp","args":["--ok"]}}
              ]
            }"#,
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        assert!(errors.is_empty(), "errors={errors:?}");
        assert!(scoped.contains_key("plugin:toolbox:default"));
        assert!(scoped.contains_key("plugin:toolbox:fromFile"));
        let inline = scoped.get("plugin:toolbox:inline").expect("inline");
        let expected_command = root.join("inline-mcp").to_string_lossy().to_string();
        assert_eq!(inline.command.as_deref(), Some(expected_command.as_str()));
        assert_eq!(inline.args, vec!["--ok".to_string()]);
        assert_eq!(
            inline.env.get("CLAUDE_PLUGIN_ROOT").map(String::as_str),
            Some(root.to_string_lossy().as_ref())
        );
        assert!(
            inline
                .env
                .get("CLAUDE_PLUGIN_DATA")
                .is_some_and(|value| value.ends_with("toolbox-inline"))
        );
        assert_eq!(inline.scope, ConfigScope::Dynamic);
        assert_eq!(inline.plugin_source.as_deref(), Some("toolbox@inline"));
    }

    #[test]
    fn plugin_mcp_user_config_variables_are_substituted_and_missing_refs_are_reported() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("user-config");
        let config_home = temp_dir("user-config-settings");
        let plugin_name = format!("toolbox-{}", uuid::Uuid::new_v4().simple());
        let plugin_source = format!("{plugin_name}@inline");
        write_file(
            &config_home.join("settings.json"),
            &serde_json::json!({
                "pluginConfigs": {
                    plugin_source.clone(): {
                        "options": {
                            "endpoint": "https://example.test/mcp",
                            "token": "plain-token"
                        }
                    }
                }
            })
            .to_string(),
        );
        let _config_home_guard = ConfigHomeGuard::pin(&config_home);
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            &serde_json::json!({
                "name": plugin_name,
                "userConfig": {
                    "endpoint": {"type":"string","title":"Value","description":"Fixture setting"},
                    "token": {"type":"string","title":"Secret","description":"Fixture secret", "sensitive": true}
                },
                "mcpServers": {
                    "configured": {
                        "type": "http",
                        "url": "${user_config.endpoint}",
                        "headers": {"Authorization": "Bearer ${user_config.token}"}
                    },
                    "missing": {
                        "type": "http",
                        "url": "${user_config.missing}"
                    }
                }
            })
            .to_string(),
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, &plugin_source, true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        let configured_key = format!("plugin:{plugin_name}:configured");
        let configured = scoped.get(&configured_key).expect("configured server");
        assert_eq!(configured.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(
            configured.headers.get("Authorization").map(String::as_str),
            Some("Bearer plain-token")
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].source(), "missing");
        assert!(
            matches!(&errors[0], PluginError::GenericError { plugin: Some(name), .. } if name == &plugin_name)
        );
        assert!(
            get_plugin_error_message(&errors[0])
                .contains("Missing required user configuration value: missing")
        );

        let _ = std::fs::remove_dir_all(config_home);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_mcp_channel_user_config_overrides_top_level_options() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("channel-user-config");
        let config_home = temp_dir("channel-user-config-settings");
        let plugin_name = format!("toolbox-{}", uuid::Uuid::new_v4().simple());
        let plugin_source = format!("{plugin_name}@inline");
        write_file(
            &config_home.join("settings.json"),
            &serde_json::json!({
                "pluginConfigs": {
                    plugin_source.clone(): {
                        "options": {
                            "endpoint": "https://top-level.test/mcp",
                            "token": "top-token"
                        },
                        "mcpServers": {
                            "configured": {
                                "endpoint": "https://channel.test/mcp",
                                "token": "plain-channel-token"
                            }
                        }
                    }
                }
            })
            .to_string(),
        );
        let server_secret_key = format!("{plugin_source}/configured");
        write_file(
            &config_home.join(".credentials.json"),
            &serde_json::json!({
                "pluginSecrets": {
                    server_secret_key: {
                        "token": "secure-channel-token"
                    }
                }
            })
            .to_string(),
        );
        let _config_home_guard = ConfigHomeGuard::pin(&config_home);
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            &serde_json::json!({
                "name": plugin_name,
                "userConfig": {
                    "endpoint": {"type":"string","title":"Value","description":"Fixture setting"},
                    "token": {"type":"string","title":"Secret","description":"Fixture secret", "sensitive": true}
                },
                "channels": [{
                    "server": "configured",
                    "displayName": "Configured",
                    "userConfig": {
                        "endpoint": {"type":"string","title":"Value","description":"Fixture setting"},
                        "token": {"type":"string","title":"Secret","description":"Fixture secret", "sensitive": true}
                    }
                }],
                "mcpServers": {
                    "configured": {
                        "type": "http",
                        "url": "${user_config.endpoint}",
                        "headers": {"Authorization": "Bearer ${user_config.token}"}
                    }
                }
            })
            .to_string(),
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, &plugin_source, true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        assert!(errors.is_empty(), "errors={errors:?}");
        let configured_key = format!("plugin:{plugin_name}:configured");
        let configured = scoped.get(&configured_key).expect("configured server");
        assert_eq!(configured.url.as_deref(), Some("https://channel.test/mcp"));
        assert_eq!(
            configured.headers.get("Authorization").map(String::as_str),
            Some("Bearer secure-channel-token")
        );

        let _ = std::fs::remove_dir_all(config_home);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_mcp_env_variables_expand_defaults_and_report_missing_without_dropping_server() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("COMETIX_PLUGIN_MCP_URL");
        crate::utils::process_env::remove("COMETIX_PLUGIN_MCP_TOKEN");
        let root = temp_dir("env-expansion");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "mcpServers": {
                "remote": {
                  "type":"http",
                  "url":"${COMETIX_PLUGIN_MCP_URL:-https://fallback.test/mcp}",
                  "headers":{"Authorization":"Bearer ${COMETIX_PLUGIN_MCP_TOKEN}"}
                }
              }
            }"#,
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        let remote = scoped.get("plugin:toolbox:remote").expect("remote server");
        assert_eq!(remote.url.as_deref(), Some("https://fallback.test/mcp"));
        assert_eq!(
            remote.headers.get("Authorization").map(String::as_str),
            Some("Bearer ${COMETIX_PLUGIN_MCP_TOKEN}")
        );
        assert_eq!(
            errors,
            vec![PluginError::McpConfigInvalid {
                source: "plugin:toolbox".into(),
                plugin: "toolbox".into(),
                server_name: "remote".into(),
                validation_error: "Missing environment variables: COMETIX_PLUGIN_MCP_TOKEN".into(),
            }]
        );
        assert_eq!(
            get_plugin_error_message(&errors[0]),
            "MCP server remote invalid: Missing environment variables: COMETIX_PLUGIN_MCP_TOKEN"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_mcpb_manifest_path_reports_official_invalid_manifest_error() {
        let root = temp_dir("mcpb-invalid");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox","mcpServers":"./server.mcpb"}"#,
        );
        fs::write(
            root.join("server.mcpb"),
            include_bytes!("../../../tests/fixtures/oracles/mcpb-schema-0915/invalid.mcpb"),
        )
        .unwrap();
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");
        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        assert!(scoped.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], PluginError::McpbInvalidManifest { source, plugin, mcpb_path, validation_error }
            if source == "toolbox@toolbox@inline" && plugin == "toolbox" && mcpb_path == "./server.mcpb" && validation_error == "Invalid manifest: Unrecognized key(s) in object: 'unknown'")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_mcpb_real_bundle_load_needs_config_and_last_wins_match_source() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("mcpb-pipeline");
        let config_home = temp_dir("mcpb-pipeline-settings");
        let _config_home_guard = ConfigHomeGuard::pin(&config_home);
        fs::write(
            root.join("valid.mcpb"),
            include_bytes!("../../../tests/fixtures/oracles/mcpb-schema-0915/valid.mcpb"),
        )
        .unwrap();
        fs::write(
            root.join("needs.mcpb"),
            include_bytes!("../../../tests/fixtures/oracles/mcpb-schema-0915/needs-config.mcpb"),
        )
        .unwrap();
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox","mcpServers":"./valid.mcpb"}"#,
        );
        let (mut plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "{plugin_errors:?}");
        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        let server = &scoped["plugin:toolbox:probe"];
        assert_eq!(server.command.as_deref(), Some("uv"));
        assert_eq!(server.args[0], "run");
        assert!(Path::new(&server.args[1]).is_file());
        assert_eq!(server.plugin_source.as_deref(), Some("toolbox@inline"));

        plugin.manifest.mcp_servers = Some(Value::String("./needs.mcpb".into()));
        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin.clone()]);
        assert!(scoped.is_empty());
        assert!(
            errors.is_empty(),
            "needs-config is not a loading error: {errors:?}"
        );

        plugin.manifest.mcp_servers = Some(
            serde_json::json!(["./valid.mcpb", {"probe":{"command":"last-wins"}}, "./needs.mcpb"]),
        );
        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            scoped["plugin:toolbox:probe"].command.as_deref(),
            Some("last-wins")
        );
        plugin.manifest.mcp_servers = Some(Value::String("./missing.mcpb".into()));
        let (scoped, errors) = extract_mcp_servers_from_plugins_readonly(&[plugin]);
        assert!(scoped.is_empty());
        assert!(
            matches!(errors.as_slice(), [PluginError::McpbExtractFailed { source, plugin, mcpb_path, reason }]
            if source == "toolbox@toolbox@inline" && plugin == "toolbox" && mcpb_path == "./missing.mcpb" && reason == &format!("MCPB file not found: {}", root.join("missing.mcpb").display()))
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(config_home).unwrap();
    }

    #[tokio::test]
    async fn mcp_file_object_entries_matches_official_source_oracle() {
        // CC mcpPluginIntegration.ts:238–266, actual extracted function and
        // original McpServerConfigSchema: proof/mcpb-schema-0915/file-source-oracle.ts.
        let cases: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/oracles/mcpb-schema-0915/file-source-oracle.json"
        )))
        .unwrap();
        let root = temp_dir("file-object-entries");
        for case in cases.as_array().unwrap() {
            if let Some(hex) = case["input_hex"].as_str() {
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                fs::write(root.join(".mcp.json"), bytes).unwrap();
            } else {
                write_file(&root.join(".mcp.json"), case["input"].as_str().unwrap());
            }
            let actual = load_mcp_servers_from_file(&root, ".mcp.json").await;
            let expected: Option<indexmap::IndexMap<String, McpServerConfig>> =
                serde_json::from_value(case["servers"].clone()).unwrap();
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "source case {}",
                case["name"]
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn nested_mcpb_failures_append_before_their_plugin_finishes() {
        // Maps to mcpPluginIntegration.ts extractMcpServersFromPlugins outer
        // Promise.all, loadPluginMcpServers inner Promise.all, and
        // loadMcpServersFromMcpb's immediate errors.push. Plugin A remains
        // pending on its second bundle while A's first failure and B's failure
        // must already be observable in the same error array.
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Duration, timeout};

        // Match main's startup prerequisite before reqwest builds its client.
        crate::utils::tls_provider::install_crypto_provider();
        let _proxy = EnvVarGuard::set("NO_PROXY", "127.0.0.1");
        let _proxy_lower = EnvVarGuard::set("no_proxy", "127.0.0.1");
        let root = temp_dir("nested-error-order");
        let mut servers = tokio::task::JoinSet::new();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(3);
        let mut urls = Vec::new();
        let mut release = Vec::new();
        for name in ["a-fast", "a-held", "b-fast"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            urls.push(format!(
                "http://{}/{}.mcpb",
                listener.local_addr().unwrap(),
                name
            ));
            let (send, recv) = tokio::sync::oneshot::channel();
            release.push(Some(send));
            let started_tx = started_tx.clone();
            servers.spawn(async move {
                timeout(Duration::from_secs(10), async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut byte = [0; 1];
                    while !request.ends_with(b"\r\n\r\n") {
                        assert!(request.len() < 8192, "bounded HTTP fixture headers");
                        socket.read_exact(&mut byte).await.unwrap();
                        request.push(byte[0]);
                    }
                    started_tx.send(name).await.unwrap();
                    recv.await.unwrap();
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nnot-zip").await.unwrap();
                    socket.shutdown().await.unwrap();
                }).await.expect("controlled MCPB server deadline");
            });
        }
        let plugin_a = LoadedPlugin {
            name: "plugin-a".into(),
            enabled: true,
            repository: "test".into(),
            path: root.join("a"),
            manifest: crate::utils::plugins::schemas::PluginManifest {
                mcp_servers: Some(serde_json::json!([urls[0], urls[1]])),
                ..Default::default()
            },
            ..Default::default()
        };
        let plugin_b = LoadedPlugin {
            name: "plugin-b".into(),
            enabled: true,
            repository: "test".into(),
            path: root.join("b"),
            manifest: crate::utils::plugins::schemas::PluginManifest {
                mcp_servers: Some(serde_json::json!([urls[2]])),
                ..Default::default()
            },
            ..Default::default()
        };
        fs::create_dir_all(&plugin_a.path).unwrap();
        fs::create_dir_all(&plugin_b.path).unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let shared = errors.clone();
        let mut loaders = tokio::task::JoinSet::new();
        loaders.spawn(async move {
            extract_mcp_servers_from_plugins(&[plugin_a, plugin_b], &shared).await
        });
        timeout(Duration::from_secs(10), async {
            for _ in 0..3 { started_rx.recv().await.expect("all bundle requests started"); }
            for (index, expected) in [(0, "plugin-a"), (2, "plugin-b"), (1, "plugin-a")] {
                release[index].take().unwrap().send(()).unwrap();
                let expected_count = if index == 0 { 1 } else if index == 2 { 2 } else { 3 };
                loop {
                    if errors.lock().unwrap().len() == expected_count { break; }
                    tokio::task::yield_now().await;
                }
                let snapshot = errors.lock().unwrap();
                assert!(matches!(snapshot.last(), Some(PluginError::McpbExtractFailed { plugin, .. }) if plugin == expected));
                if index != 1 {
                    assert!(loaders.try_join_next().is_none(), "plugin A's held bundle is still pending");
                }
            }
            let loaded = loaders.join_next().await.unwrap().unwrap();
            assert!(loaded.is_empty());
            while let Some(result) = servers.join_next().await { result.unwrap(); }
        }).await.expect("nested MCPB completion/error-order deadline");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn plugin_file_to_dedup_retains_official_first_loaded_server() {
        // mcpPluginIntegration.ts#loadMcpServersFromFile/addPluginScopeToServers
        // feed config.ts#dedupPluginMcpServers without alphabetizing server names.
        let root = temp_dir("first-seen-order");
        fs::write(root.join(".mcp.json"), r#"{"mcpServers":{"z":{"command":"echo","env":{"CHOSEN":"z"}},"a":{"command":"echo","env":{"CHOSEN":"a"}}}}"#).unwrap();
        let servers = load_mcp_servers_from_file(&root, ".mcp.json")
            .await
            .unwrap();
        assert_eq!(
            servers.keys().map(String::as_str).collect::<Vec<_>>(),
            ["z", "a"]
        );
        let scoped = add_plugin_scope_to_servers(servers, "demo", "demo@test");
        let result = crate::services::mcp::config::dedup_plugin_mcp_servers_readonly(
            &scoped,
            &indexmap::IndexMap::new(),
        );
        assert_eq!(
            result
                .servers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["plugin:demo:z"]
        );
        assert_eq!(result.servers["plugin:demo:z"].env["CHOSEN"], "z");
        // Numeric names must follow JS own-key order before the prefix makes
        // them ordinary string names. Insertion alone is not sufficient.
        let inline: Value =
            serde_json::from_str(r#"{"10":{"command":"echo"},"2":{"command":"echo"}}"#).unwrap();
        let scoped = add_plugin_scope_to_servers(
            native_inline_mcp_servers(&inline).unwrap(),
            "demo",
            "demo@test",
        );
        assert_eq!(
            scoped.keys().map(String::as_str).collect::<Vec<_>>(),
            ["plugin:demo:2", "plugin:demo:10"]
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn plugin_activation_and_extraction_match_bun_cache_semantics() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-lifecycle-0915/plugin-oracle.json"
        ))
        .unwrap();
        let root = temp_dir("activation-cache");
        fs::write(
            root.join(".mcp.json"),
            r#"{"fresh":{"command":"${CLAUDE_PLUGIN_ROOT}/bin"}}"#,
        )
        .unwrap();
        let plugin = LoadedPlugin {
            name: "demo".into(),
            source: "demo@test".into(),
            repository: "test".into(),
            enabled: true,
            path: root.clone(),
            mcp_servers: Some(serde_json::json!({"cached":{"command":"echo"}})).into(),
            ..Default::default()
        };
        let errors = std::sync::Mutex::new(Vec::new());
        let cached = get_plugin_mcp_servers(&plugin, &errors).await.unwrap();
        let mut empty = plugin.clone();
        empty.mcp_servers = Some(serde_json::json!({})).into();
        let empty_result = get_plugin_mcp_servers(&empty, &errors).await.unwrap();
        let mut disabled = plugin.clone();
        disabled.enabled = false;
        let absent = get_plugin_mcp_servers(&disabled, &errors).await.is_none();
        let extracted =
            extract_mcp_servers_from_plugins(std::slice::from_ref(&plugin), &errors).await;
        let activation = get_plugin_mcp_servers(&plugin, &errors).await.unwrap();
        let actual = serde_json::json!({
            "cachedKeys":cached.keys().collect::<Vec<_>>(),
            "emptyKeys":empty_result.keys().collect::<Vec<_>>(),
            "disabledAbsent":absent,
            "extractedKeys":extracted.keys().collect::<Vec<_>>(),
            "rawCache":plugin.mcp_servers.snapshot(),
            "rawCachedCommand":plugin.mcp_servers.snapshot().unwrap()["fresh"]["command"],
            "activationCommand":activation["plugin:demo:fresh"].command.as_ref().unwrap().replace(root.to_str().unwrap(), "<ROOT>"),
            "errors":errors.into_inner().unwrap(),
        });
        assert_eq!(actual, oracle);
        fs::remove_dir_all(root).unwrap();
    }

    /// Synchronous borrowing adapter for the source-owned async extraction above.
    /// Clones own the future input while sharing the source object's lazy cache slots.
    fn extract_mcp_servers_from_plugins_readonly(
        plugins: &[LoadedPlugin],
    ) -> (
        indexmap::IndexMap<String, ScopedMcpServerConfig>,
        Vec<PluginError>,
    ) {
        let captured = plugins.to_vec();
        crate::utils::process_runtime::block_on_from_sync(async move {
            let errors = std::sync::Mutex::new(Vec::new());
            let servers = extract_mcp_servers_from_plugins(&captured, &errors).await;
            (servers, errors.into_inner().unwrap())
        })
        .expect("plugin MCP loading requires the initialized process runtime")
    }
}
