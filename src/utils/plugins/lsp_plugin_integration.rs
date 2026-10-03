//! Plugin LSP integration.
//!
//! Maps to: CC `utils/plugins/lspPluginIntegration.ts`.
//!
//! Current safety boundary: cache/read-only plugin LSP source population. This
//! reads plugin `.lsp.json` files and manifest `lspServers` entries already on
//! disk, resolves the official plugin/user/env variables, and returns scoped
//! configs for the LSP service. It does not install plugins, download bundles,
//! or write plugin configuration.

use crate::services::lsp::types::{LspServerConfig, ScopedLspServerConfig};
use crate::types::plugin::LoadedPlugin;
use crate::types::plugin::PluginError;
use crate::utils::plugins::schemas::lsp_server_config_schema;
use crate::utils::process_env::ecmascript_object_entries;
use crate::utils::zod;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Maps to CC `lspPluginIntegration.ts#loadPluginLspServers` for the read-only
/// JSON/path/direct-object subset.
pub fn load_plugin_lsp_servers_readonly(
    plugin: &LoadedPlugin,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<IndexMap<String, LspServerConfig>> {
    let mut servers = IndexMap::new();

    if let Some(default_servers) =
        load_lsp_servers_from_file_readonly(plugin, ".lsp.json", ".lsp.json", errors, true)
    {
        servers.extend(default_servers);
    }

    if let Some(spec) = plugin.manifest.lsp_servers.as_ref() {
        if let Some(manifest_servers) = load_lsp_servers_from_spec(plugin, spec, errors) {
            servers.extend(manifest_servers);
        }
    }

    (!servers.is_empty()).then_some(servers)
}

fn load_lsp_servers_from_spec(
    plugin: &LoadedPlugin,
    spec: &Value,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<IndexMap<String, LspServerConfig>> {
    match spec {
        Value::String(path) => load_lsp_servers_from_manifest_path(plugin, path, errors),
        Value::Array(items) => {
            let mut servers = IndexMap::new();
            for item in items {
                if let Some(item_servers) = load_lsp_servers_from_spec(plugin, item, errors) {
                    // Maps to CC array merge order: later specs win on collision.
                    servers.extend(item_servers);
                }
            }
            (!servers.is_empty()).then_some(servers)
        }
        Value::Object(_) => parse_lsp_servers_object(plugin, spec, "manifest", errors),
        _ => None,
    }
}

fn load_lsp_servers_from_manifest_path(
    plugin: &LoadedPlugin,
    rel_path: &str,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<IndexMap<String, LspServerConfig>> {
    if validate_path_within_plugin(&plugin.path, rel_path).is_none() {
        errors.lock().unwrap().push(PluginError::LspConfigInvalid {
            source: "plugin".to_string(),
            plugin: plugin.name.clone(),
            server_name: rel_path.to_string(),
            validation_error: "Invalid path: must be relative and within plugin directory"
                .to_string(),
        });
        return None;
    }
    load_lsp_servers_from_file_readonly(plugin, rel_path, rel_path, errors, false)
}

/// Maps to CC `lspPluginIntegration.ts#loadLspServersFromManifest` file branch.
fn load_lsp_servers_from_file_readonly(
    plugin: &LoadedPlugin,
    relative_path: &str,
    source_name: &str,
    errors: &std::sync::Mutex<Vec<PluginError>>,
    optional: bool,
) -> Option<IndexMap<String, LspServerConfig>> {
    let Some(file_path) = validate_path_within_plugin(&plugin.path, relative_path) else {
        return None;
    };
    let content = match fs::read_to_string(&file_path) {
        Ok(content) => content,
        Err(error) => {
            if !optional || error.kind() != std::io::ErrorKind::NotFound {
                errors.lock().unwrap().push(PluginError::LspConfigInvalid {
                    source: "plugin".to_string(),
                    plugin: plugin.name.clone(),
                    server_name: source_name.to_string(),
                    validation_error: format!("Failed to parse JSON: {error}"),
                });
            }
            return None;
        }
    };
    let parsed =
        match crate::utils::slow_operations::json_parse(&content).map(|parsed| parsed.to_json()) {
            Ok(value) => value,
            Err(error) => {
                errors.lock().unwrap().push(PluginError::LspConfigInvalid {
                    source: "plugin".to_string(),
                    plugin: plugin.name.clone(),
                    server_name: source_name.to_string(),
                    validation_error: format!("Failed to parse JSON: {error}"),
                });
                return None;
            }
        };
    // CC validates a file as one record: one invalid entry rejects the entire
    // file. Inline declarations below deliberately retain per-server handling.
    let validated = match zod::safe_parse(&zod::record(lsp_server_config_schema().clone()), &parsed)
    {
        Ok(validated) => validated,
        Err(error) => {
            errors.lock().unwrap().push(PluginError::LspConfigInvalid {
                source: "plugin".to_string(),
                plugin: plugin.name.clone(),
                server_name: source_name.to_string(),
                validation_error: error.message(),
            });
            return None;
        }
    };
    let servers: IndexMap<String, LspServerConfig> = serde_json::from_value(validated)
        .expect("canonical LSP schema output fits the typed carrier");
    (!servers.is_empty()).then_some(servers)
}

/// Maps to CC `lspPluginIntegration.ts#validatePathWithinPlugin`.
fn validate_path_within_plugin(plugin_path: &Path, relative_path: &str) -> Option<PathBuf> {
    let plugin_root = normalize_absolute_path(plugin_path);
    let requested = Path::new(relative_path);
    let resolved_file = if requested.is_absolute() {
        normalize_absolute_path(requested)
    } else {
        normalize_absolute_path(&plugin_path.join(requested))
    };
    resolved_file
        .starts_with(&plugin_root)
        .then_some(resolved_file)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn parse_lsp_servers_object(
    plugin: &LoadedPlugin,
    value: &Value,
    _source_name: &str,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<IndexMap<String, LspServerConfig>> {
    // CC Object.entries puts integer-index server names first in numeric order;
    // other names retain insertion order, deciding same-extension precedence.
    let object = value.as_object()?;
    let mut servers = IndexMap::new();
    for (name, config_value) in ecmascript_object_entries(object) {
        match zod::safe_parse(lsp_server_config_schema(), config_value) {
            Ok(validated) => {
                let config = serde_json::from_value(validated)
                    .expect("canonical LSP schema output fits the typed carrier");
                servers.insert(name.to_owned(), config);
            }
            Err(error) => errors.lock().unwrap().push(PluginError::LspConfigInvalid {
                source: "plugin".to_string(),
                plugin: plugin.name.clone(),
                server_name: name.to_owned(),
                validation_error: error.message(),
            }),
        }
    }
    (!servers.is_empty()).then_some(servers)
}

/// Maps to CC `lspPluginIntegration.ts#resolvePluginLspEnvironment`.
pub fn resolve_plugin_lsp_environment_readonly(
    mut config: LspServerConfig,
    plugin: &LoadedPlugin,
    user_config: Option<&serde_json::Map<String, Value>>,
) -> Result<(LspServerConfig, Vec<String>), String> {
    let mut missing_env_vars = Vec::new();

    config.command = resolve_plugin_lsp_string_value(
        &config.command,
        plugin,
        user_config,
        &mut missing_env_vars,
    )?;
    config.args = config
        .args
        .into_iter()
        .map(|arg| {
            resolve_plugin_lsp_string_value(&arg, plugin, user_config, &mut missing_env_vars)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let existing_env = config.env.take().unwrap_or_default();
    let mut resolved_env = BTreeMap::new();
    // Maps to CC env defaults before user env spreading.
    resolved_env.insert(
        "CLAUDE_PLUGIN_ROOT".to_string(),
        crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
            "${CLAUDE_PLUGIN_ROOT}",
            &plugin.path,
            None,
        )?,
    );
    resolved_env.insert(
        "CLAUDE_PLUGIN_DATA".to_string(),
        crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
            "${CLAUDE_PLUGIN_DATA}",
            &plugin.path,
            Some(&plugin.source),
        )?,
    );
    for (key, value) in existing_env {
        resolved_env.insert(key, value);
    }
    for (key, value) in resolved_env.clone() {
        if key == "CLAUDE_PLUGIN_ROOT" || key == "CLAUDE_PLUGIN_DATA" {
            continue;
        }
        let resolved =
            resolve_plugin_lsp_string_value(&value, plugin, user_config, &mut missing_env_vars)?;
        resolved_env.insert(key, resolved);
    }
    config.env = Some(resolved_env);

    config.workspace_folder = config
        .workspace_folder
        .map(|value| {
            resolve_plugin_lsp_string_value(&value, plugin, user_config, &mut missing_env_vars)
        })
        .transpose()?;

    missing_env_vars.sort();
    missing_env_vars.dedup();
    Ok((config, missing_env_vars))
}

fn resolve_plugin_lsp_string_value(
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

/// Maps to CC `lspPluginIntegration.ts#addPluginScopeToLspServers`.
pub fn add_plugin_scope_to_lsp_servers(
    servers: IndexMap<String, LspServerConfig>,
    plugin_name: &str,
) -> IndexMap<String, ScopedLspServerConfig> {
    servers
        .into_iter()
        .map(|(name, config)| {
            let scoped_name = format!("plugin:{plugin_name}:{name}");
            (
                scoped_name,
                ScopedLspServerConfig {
                    config,
                    server_name: None,
                    scope: Some("dynamic".to_string()),
                    source: Some(plugin_name.to_string()),
                },
            )
        })
        .collect()
}

/// Maps to CC `lspPluginIntegration.ts#getPluginLspServers`.
pub fn get_plugin_lsp_servers_readonly(
    plugin: &LoadedPlugin,
    errors: &std::sync::Mutex<Vec<PluginError>>,
) -> Option<IndexMap<String, ScopedLspServerConfig>> {
    if !plugin.enabled {
        return None;
    }

    // A populated cache is already the loader's unresolved config object.
    // Source `plugin.lspServers || await load...` treats {} as present; do not
    // revalidate it or fall through to disk when it is empty.
    let servers: IndexMap<String, LspServerConfig> = match plugin.lsp_servers.snapshot() {
        Some(value) => serde_json::from_value(value).ok(),
        None => load_plugin_lsp_servers_readonly(plugin, errors),
    }?;

    // Maps to CC guard on `plugin.manifest.userConfig` before loading options.
    let user_config = plugin.manifest.user_config.as_ref().map(|_| {
        crate::utils::plugins::plugin_options_storage::load_plugin_options(
            &crate::utils::plugins::plugin_options_storage::get_plugin_storage_id(plugin),
        )
    });

    let mut resolved_servers = IndexMap::new();
    for (name, config) in servers {
        match resolve_plugin_lsp_environment_readonly(config, plugin, user_config.as_ref()) {
            Ok((config, _missing_env_vars)) => {
                // Maps to CC `resolvePluginLspEnvironment(...)`: missing
                // general env vars are logged for diagnostics but are not
                // appended to the plugin errors array and do not disable the
                // server. The original `${VAR}` token remains in the value.
                resolved_servers.insert(name, config);
            }
            Err(error) => {
                // Existing partial boundary: CC getPluginLspServers lets this
                // exception reach services/lsp/config.ts; this read-only API
                // still records its original generic diagnostic and returns None.
                errors.lock().unwrap().push(PluginError::GenericError {
                    source: name,
                    plugin: None,
                    error,
                });
                return None;
            }
        }
    }

    Some(add_plugin_scope_to_lsp_servers(
        resolved_servers,
        &plugin.name,
    ))
}

/// Maps to CC `lspPluginIntegration.ts#extractLspServersFromPlugins`.
pub fn extract_lsp_servers_from_plugins_readonly(
    plugins: &[LoadedPlugin],
) -> (IndexMap<String, ScopedLspServerConfig>, Vec<PluginError>) {
    let mut all_servers = IndexMap::new();
    let errors = std::sync::Mutex::new(Vec::new());

    for plugin in plugins.iter().filter(|plugin| plugin.enabled) {
        let Some(servers) = load_plugin_lsp_servers_readonly(plugin, &errors) else {
            continue;
        };
        plugin.lsp_servers.set(Some(
            serde_json::to_value(&servers).expect("LSP configuration serializes"),
        ));
        all_servers.extend(add_plugin_scope_to_lsp_servers(servers, &plugin.name));
    }

    (all_servers, errors.into_inner().unwrap())
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
            "cometix-plugin-lsp-{label}-{}",
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

    #[test]
    fn lsp_cache_reads_and_extract_assignments_match_official_shared_slots() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        let root = temp_dir("shared-slot");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"shared"}"#,
        );
        write_file(
            &root.join(".lsp.json"),
            r#"{"first":{"command":"${CLAUDE_PLUGIN_ROOT}/bin","extensionToLanguage":{".rs":"rust"}}}"#,
        );
        let (plugin, errors) = create_plugin_from_path(&root, "shared@inline", true, "shared");
        assert!(errors.is_empty());
        let cached = plugin.clone();
        let errors = std::sync::Mutex::new(Vec::new());
        assert!(get_plugin_lsp_servers_readonly(&plugin, &errors).is_some());
        assert!(
            cached.lsp_servers.is_none(),
            "getPluginLspServers does not fill the cache"
        );
        let (extracted, diagnostics) =
            extract_lsp_servers_from_plugins_readonly(std::slice::from_ref(&plugin));
        assert!(diagnostics.is_empty());
        assert!(extracted.contains_key("plugin:shared:first"));
        assert_eq!(
            cached.lsp_servers.snapshot().unwrap()["first"]["command"],
            "${CLAUDE_PLUGIN_ROOT}/bin",
            "cache retains unresolved source values"
        );
        write_file(&root.join(".lsp.json"), "not JSON");
        assert!(
            get_plugin_lsp_servers_readonly(&cached, &errors).is_some(),
            "getter uses populated cache without parsing disk"
        );
        assert!(errors.lock().unwrap().is_empty());
        let mut empty = plugin.clone();
        empty.lsp_servers = Some(serde_json::json!({})).into();
        assert!(
            get_plugin_lsp_servers_readonly(&empty, &errors)
                .unwrap()
                .is_empty(),
            "{{}} is a valid populated cache"
        );
        assert!(errors.lock().unwrap().is_empty());
        let (_, diagnostics) =
            extract_lsp_servers_from_plugins_readonly(std::slice::from_ref(&plugin));
        assert!(
            !diagnostics.is_empty(),
            "extract loads directly despite a populated cache"
        );
        assert!(
            cached
                .lsp_servers
                .snapshot()
                .unwrap()
                .get("first")
                .is_some(),
            "None does not overwrite the previous slot"
        );
        write_file(
            &root.join(".lsp.json"),
            r#"{"second":{"command":"next","extensionToLanguage":{".rs":"rust"}}}"#,
        );
        extract_lsp_servers_from_plugins_readonly(std::slice::from_ref(&plugin));
        assert!(
            cached
                .lsp_servers
                .snapshot()
                .unwrap()
                .get("second")
                .is_some()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_lsp_raw_number_projection_and_numeric_names_match_actual_bun_loader() {
        // Original loadPluginLspServers oracle: lsp-number-and-order-bun.json.
        // Keep raw text through the source jsonParse owner; serde's default
        // decimal parser can round the safe-integer boundary incorrectly.
        let root = temp_dir("number-projection");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"plugin"}"#,
        );
        let (mut plugin, _) = create_plugin_from_path(&root, "plugin@test", true, "plugin");
        for field in ["maxRestarts", "startupTimeout", "shutdownTimeout"] {
            for (raw, expected) in [
                ("1.0", Some(1)),
                ("1e3", Some(1000)),
                ("-0", Some(0)),
                ("4294967296.0", Some(4294967296)),
                ("9007199254740991.0", Some(9007199254740991)),
                ("1.5", None),
                ("-1", None),
                ("9007199254740992.0", None),
            ] {
                let content = format!(
                    r#"{{"server":{{"command":"x","extensionToLanguage":{{".x":"x"}},"{field}":{raw}}}}}"#
                );
                for inline in [false, true] {
                    write_file(
                        &root.join(".lsp.json"),
                        if inline { "{}" } else { &content },
                    );
                    plugin.manifest.lsp_servers = inline.then(|| {
                        crate::utils::slow_operations::json_parse(&content)
                            .unwrap()
                            .to_json()
                    });
                    let errors = std::sync::Mutex::new(Vec::new());
                    let servers = load_plugin_lsp_servers_readonly(&plugin, &errors);
                    // Only maxRestarts accepts zero; timeout positivity remains
                    // the canonical source schema's responsibility.
                    let expected = expected.filter(|n| *n > 0 || field == "maxRestarts");
                    if let Some(expected) = expected {
                        assert!(
                            errors.lock().unwrap().is_empty(),
                            "{field}={raw}, inline={inline}: {errors:?}"
                        );
                        let server = &servers.unwrap()["server"];
                        let actual = match field {
                            "maxRestarts" => server.max_restarts,
                            "startupTimeout" => server.startup_timeout,
                            _ => server.shutdown_timeout,
                        };
                        assert_eq!(actual, Some(expected), "{field}={raw}, inline={inline}");
                    } else {
                        assert!(servers.is_none(), "{field}={raw}, inline={inline}");
                        assert_eq!(
                            errors.lock().unwrap().len(),
                            1,
                            "{field}={raw}, inline={inline}"
                        );
                        assert!(
                            matches!(&errors.lock().unwrap()[0], PluginError::LspConfigInvalid { source, plugin, server_name, validation_error } if source == "plugin" && plugin == "plugin" && server_name == (if inline { "server" } else { ".lsp.json" }) && !validation_error.is_empty())
                        );
                    }
                }
            }
        }
        let raw = r#"{"10":{"command":"x","extensionToLanguage":{".x":"x"}},"2":{"command":"x","extensionToLanguage":{".x":"x"}}}"#;
        for inline in [false, true] {
            write_file(&root.join(".lsp.json"), if inline { "{}" } else { raw });
            plugin.manifest.lsp_servers = inline.then(|| {
                crate::utils::slow_operations::json_parse(raw)
                    .unwrap()
                    .to_json()
            });
            let errors = std::sync::Mutex::new(Vec::new());
            let servers = load_plugin_lsp_servers_readonly(&plugin, &errors).unwrap();
            assert!(errors.lock().unwrap().is_empty());
            assert_eq!(
                servers.keys().map(String::as_str).collect::<Vec<_>>(),
                ["2", "10"]
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_manifest_raw_numbers_reach_loader_without_precision_loss() {
        // CC pluginLoader.ts:1165 loadPluginManifest → jsonParse; then the
        // inline LSP consumer receives the parsed Number without reparsing.
        let root = temp_dir("manifest-number-projection");
        for raw in ["1.0", "1e3", "4294967296.0", "9007199254740991.0"] {
            write_file(
                &root.join(".claude-plugin/plugin.json"),
                &format!(
                    r#"{{"name":"plugin","lspServers":{{"server":{{"command":"x","extensionToLanguage":{{".x":"x"}},"maxRestarts":{raw},"startupTimeout":{raw},"shutdownTimeout":{raw}}}}}}}"#
                ),
            );
            let (plugin, manifest_errors) =
                create_plugin_from_path(&root, "plugin@test", true, "plugin");
            assert!(manifest_errors.is_empty(), "{raw}: {manifest_errors:?}");
            let errors = std::sync::Mutex::new(Vec::new());
            let servers = load_plugin_lsp_servers_readonly(&plugin, &errors).unwrap();
            assert!(errors.lock().unwrap().is_empty(), "{raw}: {errors:?}");
            let expected = match raw {
                "1.0" => 1,
                "1e3" => 1000,
                "4294967296.0" => 4294967296,
                "9007199254740991.0" => 9007199254740991,
                _ => unreachable!(),
            };
            assert_eq!(servers["server"].max_restarts, Some(expected), "{raw}");
            assert_eq!(servers["server"].startup_timeout, Some(expected), "{raw}");
            assert_eq!(servers["server"].shutdown_timeout, Some(expected), "{raw}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_file_atomic_and_inline_partial_gates_match_actual_bun_loader() {
        let root = temp_dir("canonical-gate");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"plugin"}"#,
        );
        let (mut plugin, _) = create_plugin_from_path(&root, "plugin@test", true, "plugin");
        let good = serde_json::json!({"command":"\t","extensionToLanguage":{".rs":"rust"},"maxRestarts":4294967296_u64});
        let bad = serde_json::json!({"command":"bad command","extensionToLanguage":{".rs":"rust"}});
        write_file(
            &root.join(".lsp.json"),
            &serde_json::json!({"good":good,"bad":bad}).to_string(),
        );
        let errors = std::sync::Mutex::new(Vec::new());
        assert!(load_plugin_lsp_servers_readonly(&plugin, &errors).is_none());
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(errors.lock().unwrap()[0].source(), "plugin");
        assert!(
            matches!(&errors.lock().unwrap()[0], PluginError::LspConfigInvalid { plugin, server_name, .. } if plugin == "plugin" && server_name == ".lsp.json")
        );
        assert!(
            get_plugin_error_message(&errors.lock().unwrap()[0])
                .contains("Command should not contain spaces")
        );
        fs::remove_file(root.join(".lsp.json")).unwrap();
        plugin.manifest.lsp_servers = Some(serde_json::json!({"good":good,"bad":bad}));
        errors.lock().unwrap().clear();
        let servers = load_plugin_lsp_servers_readonly(&plugin, &errors).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers["good"].command, "\t");
        assert_eq!(servers["good"].transport.as_deref(), Some("stdio"));
        assert_eq!(servers["good"].max_restarts, Some(4294967296_u64));
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert_eq!(errors.lock().unwrap()[0].source(), "plugin");
        assert!(
            matches!(&errors.lock().unwrap()[0], PluginError::LspConfigInvalid { plugin, server_name, .. } if plugin == "plugin" && server_name == "bad")
        );
        plugin.manifest.lsp_servers = Some(serde_json::json!({"":good,"  ":good}));
        errors.lock().unwrap().clear();
        let servers = load_plugin_lsp_servers_readonly(&plugin, &errors).unwrap();
        assert!(errors.lock().unwrap().is_empty());
        assert!(servers.contains_key(""));
        assert!(servers.contains_key("  "));
        plugin.manifest.lsp_servers = None;
        write_file(&root.join(".lsp.json"), "[]");
        errors.lock().unwrap().clear();
        assert!(load_plugin_lsp_servers_readonly(&plugin, &errors).is_none());
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert!(
            get_plugin_error_message(&errors.lock().unwrap()[0])
                .contains("expected record, received array")
        );
        // Actual Bun loadPluginLspServers error carrier: read and parse failures
        // share the source catch's prefix, unlike schema validation failures.
        for file in [".lsp.json", "missing.json"] {
            write_file(
                &root.join(".lsp.json"),
                if file == ".lsp.json" { "{" } else { "{}" },
            );
            plugin.manifest.lsp_servers =
                (file == "missing.json").then(|| Value::String(file.into()));
            errors.lock().unwrap().clear();
            assert!(load_plugin_lsp_servers_readonly(&plugin, &errors).is_none());
            assert_eq!(errors.lock().unwrap().len(), 1);
            assert!(
                matches!(&errors.lock().unwrap()[0], PluginError::LspConfigInvalid { source, plugin, server_name, validation_error } if source == "plugin" && plugin == "plugin" && server_name == file && validation_error.starts_with("Failed to parse JSON: "))
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_servers_merge_default_file_manifest_and_scope_names() {
        let root = temp_dir("merge");
        write_file(
            &root.join(".lsp.json"),
            r#"{"default":{"command":"default-lsp","extensionToLanguage":{".rs":"rust"}}}"#,
        );
        write_file(
            &root.join("servers.json"),
            r#"{"fromFile":{"command":"file-lsp","args":["--stdio"],"extensionToLanguage":{".ts":"typescript"}}}"#,
        );
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "lspServers":[
                "./servers.json",
                {"inline":{"command":"${CLAUDE_PLUGIN_ROOT}/inline-lsp","args":["--ok"],"extensionToLanguage":{".vue":"vue"}}}
              ]
            }"#,
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let errors = std::sync::Mutex::new(Vec::new());
        let scoped = get_plugin_lsp_servers_readonly(&plugin, &errors).expect("servers");
        assert!(errors.lock().unwrap().is_empty(), "errors={errors:?}");
        assert!(scoped.contains_key("plugin:toolbox:default"));
        assert!(scoped.contains_key("plugin:toolbox:fromFile"));
        let inline = scoped.get("plugin:toolbox:inline").expect("inline");
        let expected_command = root.join("inline-lsp").to_string_lossy().to_string();
        assert_eq!(inline.config.command, expected_command);
        assert_eq!(inline.config.args, vec!["--ok".to_string()]);
        assert_eq!(inline.config.transport.as_deref(), Some("stdio"));
        assert_eq!(inline.scope.as_deref(), Some("dynamic"));
        assert_eq!(inline.source.as_deref(), Some("toolbox"));
        assert_eq!(
            inline
                .config
                .env
                .as_ref()
                .and_then(|env| env.get("CLAUDE_PLUGIN_ROOT"))
                .map(String::as_str),
            Some(root.to_string_lossy().as_ref())
        );
        assert!(
            inline
                .config
                .env
                .as_ref()
                .and_then(|env| env.get("CLAUDE_PLUGIN_DATA"))
                .is_some_and(|value| value.ends_with("toolbox-inline"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_manifest_path_validation_allows_normalized_inside_paths_only() {
        let root = temp_dir("path-validation");
        fs::create_dir_all(root.join("configs")).unwrap();
        write_file(
            &root.join("servers.json"),
            r#"{"safe":{"command":"safe-lsp","extensionToLanguage":{".rs":"rust"}}}"#,
        );
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox"}"#,
        );
        let (mut plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");
        // Test the source integration input boundary, including malformed declarations.
        // The separate pluginLoader manifest validator must remain strict.
        plugin.manifest.lsp_servers =
            Some(serde_json::from_str(r#"["configs/../servers.json","../outside.json"]"#).unwrap());

        let errors = std::sync::Mutex::new(Vec::new());
        let scoped = get_plugin_lsp_servers_readonly(&plugin, &errors).expect("safe server");
        assert!(scoped.contains_key("plugin:toolbox:safe"));
        assert_eq!(errors.lock().unwrap().len(), 1, "errors={errors:?}");
        assert!(
            get_plugin_error_message(&errors.lock().unwrap()[0])
                .contains("must be relative and within plugin directory")
        );

        let absolute_inside = root.join("servers.json");
        assert_eq!(
            super::validate_path_within_plugin(&root, absolute_inside.to_str().unwrap()),
            Some(absolute_inside)
        );
        assert!(super::validate_path_within_plugin(&root, "/tmp/outside.json").is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_user_config_env_and_workspace_are_resolved() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = temp_dir("user-config");
        let config_home = temp_dir("user-config-settings");
        let _env = [
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home),
            EnvVarGuard::set("COMETIX_LSP_TOKEN", "env-token"),
        ];
        let plugin_name = format!("toolbox-{}", uuid::Uuid::new_v4().simple());
        let plugin_source = format!("{plugin_name}@inline");
        write_file(
            &config_home.join("settings.json"),
            &serde_json::json!({
                "pluginConfigs": {
                    plugin_source.clone(): {
                        "options": {
                            "workspace": root.join("workspace").display().to_string()
                        }
                    }
                }
            })
            .to_string(),
        );
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            &serde_json::json!({
                "name": plugin_name,
                "userConfig": {
                    "workspace": {"type":"string","title":"Value","description":"Fixture setting"}
                },
                "lspServers": {
                    "configured": {
                        "command": "${CLAUDE_PLUGIN_ROOT}/bin/lsp",
                        "args": ["--workspace", "${user_config.workspace}"],
                        "env": {"TOKEN":"${COMETIX_LSP_TOKEN}"},
                        "workspaceFolder": "${user_config.workspace}",
                        "extensionToLanguage": {".rs":"rust"}
                    }
                }
            })
            .to_string(),
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, &plugin_source, true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let errors = std::sync::Mutex::new(Vec::new());
        let scoped = get_plugin_lsp_servers_readonly(&plugin, &errors).expect("servers");
        assert!(errors.lock().unwrap().is_empty(), "errors={errors:?}");
        let key = format!("plugin:{plugin_name}:configured");
        let configured = scoped.get(&key).expect("configured");
        assert_eq!(
            configured.config.command,
            root.join("bin/lsp").display().to_string()
        );
        assert_eq!(
            configured.config.args,
            vec![
                "--workspace".to_string(),
                root.join("workspace").display().to_string()
            ]
        );
        assert_eq!(
            configured
                .config
                .env
                .as_ref()
                .and_then(|env| env.get("TOKEN"))
                .map(String::as_str),
            Some("env-token")
        );
        assert_eq!(
            configured.config.workspace_folder.as_deref(),
            Some(root.join("workspace").to_string_lossy().as_ref())
        );

        let _ = fs::remove_dir_all(config_home);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_manifest_path_traversal_and_schema_errors_are_reported() {
        let root = temp_dir("invalid");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox"}"#,
        );
        let (mut plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");
        // Test the source integration input boundary, including malformed declarations.
        // The separate pluginLoader manifest validator must remain strict.
        plugin.manifest.lsp_servers = Some(serde_json::from_str(r#"["../outside.json",{"bad":{"command":"bad command","extensionToLanguage":{"rs":"rust"}}}]"#).unwrap());

        let errors = std::sync::Mutex::new(Vec::new());
        let servers = get_plugin_lsp_servers_readonly(&plugin, &errors);
        assert!(servers.is_none());
        assert_eq!(errors.lock().unwrap().len(), 2);
        assert_eq!(
            errors.lock().unwrap()[0],
            PluginError::LspConfigInvalid {
                source: "plugin".into(),
                plugin: "toolbox".into(),
                server_name: "../outside.json".into(),
                validation_error: "Invalid path: must be relative and within plugin directory"
                    .into(),
            }
        );
        assert!(
            get_plugin_error_message(&errors.lock().unwrap()[1])
                .contains("Command should not contain spaces")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_strict_config_rejects_unknown_keys_like_official_schema() {
        let root = temp_dir("strict");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox"}"#,
        );
        let (mut plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");
        // Test the source integration input boundary, including malformed declarations.
        // The separate pluginLoader manifest validator must remain strict.
        plugin.manifest.lsp_servers = Some(serde_json::from_str(r#"{"bad":{"command":"toolbox-lsp","extensionToLanguage":{".rs":"rust"},"extraField":true}}"#).unwrap());

        let errors = std::sync::Mutex::new(Vec::new());
        let servers = get_plugin_lsp_servers_readonly(&plugin, &errors);
        assert!(servers.is_none());
        assert_eq!(errors.lock().unwrap().len(), 1);
        assert!(get_plugin_error_message(&errors.lock().unwrap()[0]).contains("Unrecognized key"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_lsp_missing_general_env_vars_do_not_disable_server_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("COMETIX_LSP_MISSING_ENV");
        let root = temp_dir("missing-env");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "lspServers": {
                "server": {
                  "command":"toolbox-lsp",
                  "args":["${COMETIX_LSP_MISSING_ENV}"],
                  "extensionToLanguage":{".rs":"rust"}
                }
              }
            }"#,
        );
        let (plugin, plugin_errors) =
            create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(plugin_errors.is_empty(), "errors={plugin_errors:?}");

        let errors = std::sync::Mutex::new(Vec::new());
        let servers = get_plugin_lsp_servers_readonly(&plugin, &errors).expect("servers");
        assert!(
            errors.lock().unwrap().is_empty(),
            "missing env vars are log-only: {errors:?}"
        );
        assert_eq!(
            servers["plugin:toolbox:server"].config.args,
            vec!["${COMETIX_LSP_MISSING_ENV}".to_string()]
        );
        let _ = fs::remove_dir_all(root);
    }
}
