//! Maps to: CC `services/plugins/pluginOperations.ts`.
use crate::types::plugin::LoadedPlugin;
use crate::utils::plugins::{
    cache_utils::*,
    dependency_resolver::*,
    installed_plugins_manager::*,
    marketplace_manager::*,
    plugin_identifier::*,
    plugin_installation_helpers::*,
    plugin_loader::*,
    plugin_policy::*,
    plugin_startup_check::*,
    plugin_versioning::*,
    schemas::{PluginManifest, PluginScope},
};
use crate::utils::settings::{
    constants::SettingSource, get_settings_for_source, update_settings_for_source,
};
use serde_json::{Map, Value};
use std::path::Path;
/// Maps to: CC pluginOperations.ts:75-87.
pub const VALID_INSTALLABLE_SCOPES: [PluginScope; 3] =
    [PluginScope::User, PluginScope::Project, PluginScope::Local];
pub const VALID_UPDATE_SCOPES: [PluginScope; 4] = [
    PluginScope::User,
    PluginScope::Project,
    PluginScope::Local,
    PluginScope::Managed,
];
pub type InstallableScope = PluginScope;
/// Maps to: CC pluginOperations.ts:90-98#assertInstallableScope.
pub fn assert_installable_scope(scope: &str) -> anyhow::Result<PluginScope> {
    match scope {
        "user" => Ok(PluginScope::User),
        "project" => Ok(PluginScope::Project),
        "local" => Ok(PluginScope::Local),
        _ => anyhow::bail!("Invalid scope \"{scope}\". Must be one of: user, project, local"),
    }
}
/// Maps to: CC pluginOperations.ts:104-108#isInstallableScope.
pub fn is_installable_scope(scope: PluginScope) -> bool {
    VALID_INSTALLABLE_SCOPES.contains(&scope)
}
// Source scope string union projected by the existing enum at Rust boundaries.
fn scope_name(scope: PluginScope) -> &'static str {
    match scope {
        PluginScope::User => "user",
        PluginScope::Project => "project",
        PluginScope::Local => "local",
        PluginScope::Managed => "managed",
    }
}
/// Maps to: CC pluginOperations.ts:114-116#getProjectPathForScope.
pub fn get_project_path_for_scope(scope: PluginScope) -> Option<String> {
    matches!(scope, PluginScope::Project | PluginScope::Local).then(|| {
        crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string()
    })
}
/// Maps to: CC pluginOperations.ts:128-132#isPluginEnabledAtProjectScope.
pub fn is_plugin_enabled_at_project_scope(id: &str) -> bool {
    get_settings_for_source(SettingSource::Project)
        .and_then(|s| s.enabled_plugins)
        .is_some_and(|v| v[id] == Value::Bool(true))
}
/// Maps to: CC pluginOperations.ts:146-170 result types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginOperationResult {
    pub success: bool,
    pub message: String,
    pub plugin_id: Option<String>,
    pub plugin_name: Option<String>,
    pub scope: Option<PluginScope>,
    pub reverse_dependents: Option<Vec<String>>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginUpdateResult {
    pub success: bool,
    pub message: String,
    pub plugin_id: Option<String>,
    pub new_version: Option<String>,
    pub old_version: Option<String>,
    pub already_up_to_date: Option<bool>,
    pub scope: Option<PluginScope>,
}
/// Maps to: CC pluginOperations.ts:180-201#findPluginInSettings.
fn find_plugin_in_settings(plugin: &str) -> Option<(String, PluginScope)> {
    for scope in [PluginScope::Local, PluginScope::Project, PluginScope::User] {
        let entries =
            get_settings_for_source(SettingSource::from(scope_to_setting_source(scope).unwrap()))
                .and_then(|s| s.enabled_plugins)
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
        for key in entries.keys() {
            if if plugin.contains('@') {
                key == plugin
            } else {
                key.starts_with(&format!("{plugin}@"))
            } {
                return Some((key.clone(), scope));
            }
        }
    }
    None
}
/// Maps to: CC pluginOperations.ts:206-223#findPluginByIdentifier.
fn find_plugin_by_identifier<'a>(
    plugin: &str,
    plugins: &'a [LoadedPlugin],
) -> Option<&'a LoadedPlugin> {
    let parsed = parse_plugin_identifier(plugin);
    plugins.iter().find(|p| {
        p.name == plugin
            || p.name == parsed.name
            || parsed.marketplace.as_ref().is_some_and(|m| {
                !m.is_empty() && p.name == parsed.name && p.source.contains(&format!("@{m}"))
            })
    })
}
/// Maps to: CC pluginOperations.ts:230-251#resolveDelistedPluginId.
fn resolve_delisted_plugin_id(plugin: &str) -> Option<(String, String)> {
    let name = parse_plugin_identifier(plugin).name;
    let data = load_installed_plugins_v2();
    let data = data.lock().unwrap();
    let entries = data["plugins"].as_object()?;
    if entries
        .get(plugin)
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return Some((plugin.into(), name));
    }
    entries
        .iter()
        .find(|(key, value)| {
            parse_plugin_identifier(key).name == name
                && value.as_array().is_some_and(|a| !a.is_empty())
        })
        .map(|(key, _)| (key.clone(), name))
}
/// Maps to: CC pluginOperations.ts:258-299#getPluginInstallationFromV2 return object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInstallationScope {
    pub scope: PluginScope,
    pub project_path: Option<String>,
}
pub fn get_plugin_installation_from_v2(id: &str) -> PluginInstallationScope {
    let data = load_installed_plugins_v2();
    let data = data.lock().unwrap();
    let entries = data["plugins"][id].as_array();
    let default = PluginInstallationScope {
        scope: PluginScope::User,
        project_path: None,
    };
    let Some(entries) = entries.filter(|v| !v.is_empty()) else {
        return default;
    };
    let cwd = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    for scope in ["local", "project", "user"] {
        if let Some(entry) = entries.iter().find(|e| {
            e["scope"].as_str() == Some(scope)
                && (scope == "user" || e["projectPath"].as_str() == Some(&cwd))
        }) {
            return PluginInstallationScope {
                scope: serde_json::from_value(entry["scope"].clone()).unwrap_or(PluginScope::User),
                project_path: if scope == "user" {
                    None
                } else {
                    entry["projectPath"].as_str().map(str::to_owned)
                },
            };
        }
    }
    let entry = &entries[0];
    PluginInstallationScope {
        scope: serde_json::from_value(entry["scope"].clone()).unwrap_or(PluginScope::User),
        project_path: entry["projectPath"].as_str().map(str::to_owned),
    }
}
/// Maps to: CC pluginOperations.ts:321-418#installPluginOp.
pub async fn install_plugin_op(
    plugin: &str,
    scope: PluginScope,
) -> anyhow::Result<PluginOperationResult> {
    assert_installable_scope(scope_name(scope))?;
    let parsed = parse_plugin_identifier(plugin);
    let mut found = None;
    if parsed.marketplace.as_ref().is_some_and(|s| !s.is_empty()) {
        found = get_plugin_by_id(plugin).await.map(|info| {
            let id = format!(
                "{}@{}",
                info.entry["name"].as_str().unwrap_or_default(),
                parsed.marketplace.as_deref().unwrap()
            );
            (info.entry, info.marketplace_install_location, id)
        });
    } else {
        for (name, config) in load_known_marketplaces_config().await? {
            match get_marketplace(&name).await {
                Ok(marketplace) => {
                    if let Some(entry) = marketplace["plugins"].as_array().and_then(|entries| {
                        entries
                            .iter()
                            .find(|entry| entry["name"].as_str() == Some(parsed.name.as_str()))
                    }) {
                        found = Some((
                            entry.clone(),
                            config.get("installLocation").cloned(),
                            format!("{}@{name}", entry["name"].as_str().unwrap_or_default()),
                        ));
                        break;
                    }
                }
                Err(error) => crate::utils::log::log_error(crate::utils::log::LogError::new(
                    error.to_string(),
                )),
            }
        }
    }
    let Some((entry, location, id)) = found else {
        return Ok(PluginOperationResult {
            message: format!(
                "Plugin \"{}\" not found in {}",
                parsed.name,
                parsed
                    .marketplace
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("marketplace \"{name}\""))
                    .unwrap_or_else(|| "any configured marketplace".into())
            ),
            ..Default::default()
        });
    };
    let result = install_resolved_plugin(&id, &entry, scope, location.as_ref()).await?;
    let failure = match result {
        InstallCoreResult::Success { dep_note, .. } => {
            return Ok(PluginOperationResult {
                success: true,
                message: format!(
                    "Successfully installed plugin: {id} (scope: {}){dep_note}",
                    scope_name(scope)
                ),
                plugin_id: Some(id),
                plugin_name: entry["name"].as_str().map(str::to_owned),
                scope: Some(scope),
                ..Default::default()
            });
        }
        InstallCoreResult::LocalSourceNoLocation { plugin_name } => format!(
            "Cannot install local plugin \"{plugin_name}\" without marketplace install location"
        ),
        InstallCoreResult::SettingsWriteFailed { message } => {
            format!("Failed to update settings: {message}")
        }
        InstallCoreResult::ResolutionFailed { resolution } => format_resolution_error(&resolution),
        InstallCoreResult::BlockedByPolicy { plugin_name } => format!(
            "Plugin \"{plugin_name}\" is blocked by your organization's policy and cannot be installed"
        ),
        InstallCoreResult::DependencyBlockedByPolicy {
            plugin_name,
            blocked_dependency,
        } => format!(
            "Plugin \"{plugin_name}\" depends on \"{blocked_dependency}\", which is blocked by your organization's policy"
        ),
    };
    Ok(PluginOperationResult {
        message: failure,
        ..Default::default()
    })
}
/// Maps to: CC pluginOperations.ts:427-558#uninstallPluginOp.
pub async fn uninstall_plugin_op(
    plugin: &str,
    scope: PluginScope,
    delete_data_dir: bool,
) -> anyhow::Result<PluginOperationResult> {
    assert_installable_scope(scope_name(scope))?;
    let loaded = load_all_plugins().await?;
    let all = loaded
        .enabled
        .into_iter()
        .chain(loaded.disabled)
        .collect::<Vec<_>>();
    let source = SettingSource::from(scope_to_setting_source(scope).unwrap());
    let settings = get_settings_for_source(source);
    let entries = settings
        .and_then(|s| s.enabled_plugins)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let (id, name) = if let Some(found) = find_plugin_by_identifier(plugin, &all) {
        (
            entries
                .keys()
                .find(|key| {
                    key.as_str() == plugin
                        || key.as_str() == found.name
                        || key.starts_with(&format!("{}@", found.name))
                })
                .cloned()
                .unwrap_or_else(|| {
                    if plugin.contains('@') {
                        plugin.into()
                    } else {
                        found.name.clone()
                    }
                }),
            found.name.clone(),
        )
    } else {
        match resolve_delisted_plugin_id(plugin) {
            Some(found) => found,
            None => {
                return Ok(PluginOperationResult {
                    message: format!("Plugin \"{plugin}\" not found in installed plugins"),
                    ..Default::default()
                });
            }
        }
    };
    let project = get_project_path_for_scope(scope);
    let data = load_installed_plugins_v2();
    let installs = data.lock().unwrap()["plugins"][&id]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let installation = installs.iter().find(|i| {
        i["scope"].as_str() == Some(scope_name(scope))
            && i["projectPath"].as_str() == project.as_deref()
    });
    let Some(installation) = installation else {
        let actual = get_plugin_installation_from_v2(&id).scope;
        let message = if actual != scope && !installs.is_empty() {
            if actual == PluginScope::Project {
                format!(
                    "Plugin \"{plugin}\" is enabled at project scope (.claude/settings.json, shared with your team). To disable just for you: claude plugin disable {plugin} --scope local"
                )
            } else {
                format!(
                    "Plugin \"{plugin}\" is installed in {} scope, not {}. Use --scope {} to uninstall.",
                    scope_name(actual),
                    scope_name(scope),
                    scope_name(actual)
                )
            }
        } else {
            format!(
                "Plugin \"{plugin}\" is not installed in {} scope. Use --scope to specify the correct scope.",
                scope_name(scope)
            )
        };
        return Ok(PluginOperationResult {
            message,
            ..Default::default()
        });
    };
    let install_path = installation["installPath"].as_str().map(str::to_owned);
    let mut changed = entries;
    changed.insert(id.clone(), Value::Null);
    let _ = update_settings_for_source(
        source,
        &Map::from_iter([("enabledPlugins".into(), Value::Object(changed))]),
    );
    clear_all_caches();
    remove_plugin_installation(&id, scope, project.as_deref())?;
    let last = load_installed_plugins_v2().lock().unwrap()["plugins"][&id]
        .as_array()
        .is_none_or(Vec::is_empty);
    if last {
        if let Some(path) = install_path.filter(|s| !s.is_empty()) {
            mark_plugin_version_orphaned(Path::new(&path)).await;
        }
        crate::utils::plugins::plugin_options_storage::delete_plugin_options(&id);
        if delete_data_dir {
            crate::utils::plugins::plugin_directories::delete_plugin_data_dir(&id).await;
        }
    }
    let deps = find_reverse_dependents(&id, &all);
    let suffix = format_reverse_dependents_suffix(Some(&deps));
    Ok(PluginOperationResult {
        success: true,
        message: format!(
            "Successfully uninstalled plugin: {name} (scope: {}){suffix}",
            scope_name(scope)
        ),
        plugin_id: Some(id),
        plugin_name: Some(name),
        scope: Some(scope),
        reverse_dependents: (!deps.is_empty()).then_some(deps),
    })
}
/// Maps to: CC pluginOperations.ts:573-747#setPluginEnabledOp.
pub async fn set_plugin_enabled_op(
    plugin: &str,
    enabled: bool,
    scope: Option<PluginScope>,
) -> anyhow::Result<PluginOperationResult> {
    let operation = if enabled { "enable" } else { "disable" };
    if crate::plugins::builtin_plugins::is_builtin_plugin_id(plugin) {
        let mut entries = get_settings_for_source(SettingSource::User)
            .and_then(|s| s.enabled_plugins)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        entries.insert(plugin.into(), Value::Bool(enabled));
        if let Err(error) = update_settings_for_source(
            SettingSource::User,
            &Map::from_iter([("enabledPlugins".into(), Value::Object(entries))]),
        ) {
            return Ok(PluginOperationResult {
                message: format!("Failed to {operation} built-in plugin: {error}"),
                ..Default::default()
            });
        }
        clear_all_caches();
        let name = parse_plugin_identifier(plugin).name;
        return Ok(PluginOperationResult {
            success: true,
            message: format!("Successfully {operation}d built-in plugin: {name}"),
            plugin_id: Some(plugin.into()),
            plugin_name: Some(name),
            scope: Some(PluginScope::User),
            ..Default::default()
        });
    }
    if let Some(scope) = scope {
        assert_installable_scope(scope_name(scope))?;
    }
    let found = find_plugin_in_settings(plugin);
    let (id, resolved) = if let Some(scope) = scope {
        if let Some((id, _)) = &found {
            (id.clone(), scope)
        } else if plugin.contains('@') {
            (plugin.into(), scope)
        } else {
            return Ok(PluginOperationResult {
                message: format!(
                    "Plugin \"{plugin}\" not found in settings. Use plugin@marketplace format."
                ),
                ..Default::default()
            });
        }
    } else if let Some(found) = &found {
        found.clone()
    } else if plugin.contains('@') {
        (plugin.into(), PluginScope::User)
    } else {
        return Ok(PluginOperationResult {
            message: format!(
                "Plugin \"{plugin}\" not found in any editable settings scope. Use plugin@marketplace format."
            ),
            ..Default::default()
        });
    };
    if enabled && is_plugin_blocked_by_policy(&id) {
        return Ok(PluginOperationResult {
            message: format!(
                "Plugin \"{id}\" is blocked by your organization's policy and cannot be enabled"
            ),
            ..Default::default()
        });
    }
    let source = SettingSource::from(scope_to_setting_source(resolved).unwrap());
    let mut entries = get_settings_for_source(source)
        .and_then(|s| s.enabled_plugins)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let value = entries.get(&id);
    let precedence = |scope| match scope {
        PluginScope::User => 0,
        PluginScope::Project => 1,
        PluginScope::Local => 2,
        _ => -1,
    };
    let is_override = scope
        .zip(found.as_ref())
        .is_some_and(|(scope, (_, found))| precedence(scope) > precedence(*found));
    if let (Some(scope), Some((_, found_scope))) = (scope, found.as_ref()) {
        if value.is_none() && *found_scope != scope && !is_override {
            return Ok(PluginOperationResult {
                message: format!(
                    "Plugin \"{plugin}\" is installed at {} scope, not {}. Use --scope {} or omit --scope to auto-detect.",
                    scope_name(*found_scope),
                    scope_name(scope),
                    scope_name(*found_scope)
                ),
                ..Default::default()
            });
        }
    }
    let current = if scope.is_some() && !is_override {
        value == Some(&Value::Bool(true))
    } else {
        get_plugin_editable_scopes().contains_key(&id)
    };
    if enabled == current {
        return Ok(PluginOperationResult {
            message: format!(
                "Plugin \"{plugin}\" is already {}{}",
                if enabled { "enabled" } else { "disabled" },
                scope
                    .map(|s| format!(" at {} scope", scope_name(s)))
                    .unwrap_or_default()
            ),
            ..Default::default()
        });
    }
    let deps = if !enabled {
        let loaded = load_all_plugins().await?;
        find_reverse_dependents(
            &id,
            &loaded
                .enabled
                .into_iter()
                .chain(loaded.disabled)
                .collect::<Vec<_>>(),
        )
    } else {
        vec![]
    };
    // CC rereads after the awaited reverse-dependent lookup; preserve edits
    // made while loadAllPlugins was pending instead of writing the guard snapshot.
    let mut entries = get_settings_for_source(source)
        .and_then(|s| s.enabled_plugins)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    entries.insert(id.clone(), Value::Bool(enabled));
    if let Err(error) = update_settings_for_source(
        source,
        &Map::from_iter([("enabledPlugins".into(), Value::Object(entries))]),
    ) {
        return Ok(PluginOperationResult {
            message: format!("Failed to {operation} plugin: {error}"),
            ..Default::default()
        });
    }
    clear_all_caches();
    let name = parse_plugin_identifier(&id).name;
    let suffix = format_reverse_dependents_suffix(Some(&deps));
    Ok(PluginOperationResult {
        success: true,
        message: format!(
            "Successfully {operation}d plugin: {name} (scope: {}){suffix}",
            scope_name(resolved)
        ),
        plugin_id: Some(id),
        plugin_name: Some(name),
        scope: Some(resolved),
        reverse_dependents: (!deps.is_empty()).then_some(deps),
    })
}
/// Maps to: CC pluginOperations.ts:756-761#enablePluginOp.
pub async fn enable_plugin_op(
    plugin: &str,
    scope: Option<PluginScope>,
) -> anyhow::Result<PluginOperationResult> {
    set_plugin_enabled_op(plugin, true, scope).await
}
/// Maps to: CC pluginOperations.ts:770-775#disablePluginOp.
pub async fn disable_plugin_op(
    plugin: &str,
    scope: Option<PluginScope>,
) -> anyhow::Result<PluginOperationResult> {
    set_plugin_enabled_op(plugin, false, scope).await
}
/// Maps to: CC pluginOperations.ts:782-812#disableAllPluginsOp.
pub async fn disable_all_plugins_op() -> anyhow::Result<PluginOperationResult> {
    let enabled = get_plugin_editable_scopes();
    if enabled.is_empty() {
        return Ok(PluginOperationResult {
            success: true,
            message: "No enabled plugins to disable".into(),
            ..Default::default()
        });
    }
    let mut disabled = Vec::new();
    let mut errors = Vec::new();
    for (id, _) in enabled {
        let result = set_plugin_enabled_op(&id, false, None).await?;
        if result.success {
            disabled.push(id);
        } else {
            errors.push(format!("{id}: {}", result.message));
        }
    }
    let base = format!(
        "Disabled {} {}",
        disabled.len(),
        if disabled.len() == 1 {
            "plugin"
        } else {
            "plugins"
        }
    );
    Ok(PluginOperationResult {
        success: errors.is_empty(),
        message: if errors.is_empty() {
            base
        } else {
            format!("{base}, {} failed:\n{}", errors.len(), errors.join("\n"))
        },
        ..Default::default()
    })
}
/// Maps to: CC pluginOperations.ts:829-890#updatePluginOp.
pub async fn update_plugin_op(
    plugin: &str,
    scope: PluginScope,
) -> anyhow::Result<PluginUpdateResult> {
    let parsed = parse_plugin_identifier(plugin);
    let id = parsed
        .marketplace
        .filter(|m| !m.is_empty())
        .map(|m| format!("{}@{m}", parsed.name))
        .unwrap_or_else(|| plugin.into());
    let fail = |message| PluginUpdateResult {
        message,
        plugin_id: Some(id.clone()),
        scope: Some(scope),
        ..Default::default()
    };
    let Some(info) = get_plugin_by_id(plugin).await else {
        return Ok(fail(format!("Plugin \"{}\" not found", parsed.name)));
    };
    let data = load_installed_plugins_from_disk();
    let Some(installs) = data["plugins"][&id].as_array().filter(|v| !v.is_empty()) else {
        return Ok(fail(format!("Plugin \"{}\" is not installed", parsed.name)));
    };
    let project = get_project_path_for_scope(scope);
    let Some(installation) = installs.iter().find(|i| {
        i["scope"].as_str() == Some(scope_name(scope))
            && i["projectPath"].as_str() == project.as_deref()
    }) else {
        return Ok(fail(format!(
            "Plugin \"{}\" is not installed at scope {}",
            parsed.name,
            project
                .as_ref()
                .map(|p| format!("{} ({p})", scope_name(scope)))
                .unwrap_or_else(|| scope_name(scope).into())
        )));
    };
    perform_plugin_update(
        &id,
        &parsed.name,
        &info.entry,
        info.marketplace_install_location.as_ref(),
        installation,
        scope,
        project.as_deref(),
    )
    .await
}
/// Maps to: CC pluginOperations.ts:896-1088#performPluginUpdate.
async fn perform_plugin_update(
    id: &str,
    name: &str,
    entry: &Value,
    location: Option<&Value>,
    installation: &Value,
    scope: PluginScope,
    project: Option<&str>,
) -> anyhow::Result<PluginUpdateResult> {
    let fail = |message| PluginUpdateResult {
        message,
        plugin_id: Some(id.into()),
        scope: Some(scope),
        ..Default::default()
    };
    let old = installation["version"].as_str().map(str::to_owned);
    let source = &entry["source"];
    let (source_path, version, cleanup, sha) = if let Some(local) = source.as_str() {
        let location = Path::new(
            // NodeFsOperations.stat forwards to fs/promises.stat; retain
            // Bun's actual raw-JSON argument error at this consumer.
            location
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("path must be a string or TypedArray"))?,
        );
        let metadata = match tokio::fs::metadata(location).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(fail(format!(
                    "Marketplace directory not found at {}",
                    location.display()
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let directory = if metadata.is_dir() {
            location
        } else {
            location.parent().unwrap_or(location)
        };
        let path = crate::utils::plugins::plugin_loader::node_path_join(directory, local);
        match tokio::fs::metadata(&path).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(fail(format!(
                    "Plugin source not found at {}",
                    path.display()
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let manifest = load_plugin_manifest(
            &path.join(".claude-plugin/plugin.json"),
            entry["name"].as_str().unwrap_or(name),
            local,
        )
        .await
        .ok();
        let version = calculate_plugin_version(
            id,
            source,
            manifest.as_ref(),
            Some(&path),
            entry["version"].as_str(),
            None,
        )
        .await;
        (path, version, false, None)
    } else {
        let cached = cache_plugin(
            source,
            Some(&PluginManifest {
                name: entry["name"].as_str().unwrap_or(name).into(),
                ..Default::default()
            }),
        )
        .await?;
        let version = calculate_plugin_version(
            id,
            source,
            Some(&cached.manifest),
            Some(&cached.path),
            entry["version"].as_str(),
            cached.git_commit_sha.as_deref(),
        )
        .await;
        (cached.path, version, true, cached.git_commit_sha)
    };
    let result=async{let path=get_versioned_cache_path(id,&version);let zip=get_versioned_zip_cache_path(id,&version);let base=PluginUpdateResult{success:true,plugin_id:Some(id.into()),scope:Some(scope),old_version:old.clone(),new_version:Some(version.clone()),..Default::default()};if old.as_deref()==Some(&version)||installation["installPath"].as_str().is_some_and(|p|Path::new(p)==path||Path::new(p)==zip){return Ok(PluginUpdateResult{message:format!("{name} is already at the latest version ({version})."),already_up_to_date:Some(true),..base});}let path=copy_plugin_to_versioned_cache(&source_path,id,&version,Some(entry),None).await?;update_installation_path_on_disk(id,scope,project,&path.display().to_string(),&version,sha.as_deref())?;if let Some(old_path)=installation["installPath"].as_str().filter(|p|!p.is_empty()&&Path::new(p)!=path){let data=load_installed_plugins_from_disk();let used=data["plugins"].as_object().into_iter().flatten().any(|(_,installs)|installs.as_array().into_iter().flatten().any(|i|i["installPath"].as_str()==Some(old_path)));if !used{mark_plugin_version_orphaned(Path::new(old_path)).await;}}let scope_desc=project.map(|p|format!("{} ({p})",scope_name(scope))).unwrap_or_else(||scope_name(scope).into());Ok::<_,anyhow::Error>(PluginUpdateResult{message:format!("Plugin \"{name}\" updated from {} to {version} for scope {scope_desc}. Restart to apply changes.",old.as_deref().filter(|s|!s.is_empty()).unwrap_or("unknown")),..base})}.await;
    if cleanup && source_path != get_versioned_cache_path(id, &version) {
        crate::utils::plugins::plugin_loader::remove_path_force(&source_path).await?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    #[tokio::test]
    async fn update_raw_location_type_error_matches_official_bun_stat() {
        for location in [
            None,
            Some(Value::Null),
            Some(serde_json::json!(42)),
            Some(serde_json::json!({})),
            Some(serde_json::json!([])),
            Some(Value::Bool(false)),
        ] {
            let error = perform_plugin_update(
                "p@m",
                "p",
                &serde_json::json!({"name":"p","source":"./p"}),
                location.as_ref(),
                &serde_json::json!({"installPath":"/unused","version":"1"}),
                PluginScope::User,
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "path must be a string or TypedArray");
        }
    }

    #[tokio::test]
    async fn install_not_found_messages_match_official_identifier_locations() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("plugin-op-missing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/known_marketplaces.json"), "{}").unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", root.join("plugins"));
        for (id, expected) in [
            (
                "missing",
                "Plugin \"missing\" not found in any configured marketplace",
            ),
            (
                "missing@market",
                "Plugin \"missing\" not found in marketplace \"market\"",
            ),
        ] {
            let result = install_plugin_op(id, PluginScope::User).await.unwrap();
            assert!(!result.success);
            assert_eq!(result.message, expected);
            assert_eq!(result.plugin_id, None);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identifier_resolution_preserves_official_name_first_match_even_with_marketplace() {
        let first = LoadedPlugin {
            name: "p".into(),
            source: "p@a".into(),
            ..Default::default()
        };
        let second = LoadedPlugin {
            name: "p".into(),
            source: "p@b".into(),
            ..Default::default()
        };
        assert_eq!(
            find_plugin_by_identifier("p@b", &[first, second])
                .unwrap()
                .source,
            "p@a"
        );
    }
    #[test]
    fn installable_scopes_reject_managed_but_updates_include_it() {
        assert_eq!(
            assert_installable_scope("managed").unwrap_err().to_string(),
            "Invalid scope \"managed\". Must be one of: user, project, local"
        );
        assert!(VALID_UPDATE_SCOPES.contains(&PluginScope::Managed));
        assert!(!is_installable_scope(PluginScope::Managed));
    }
}
