//! Maps to: CC `utils/plugins/pluginInstallationHelpers.ts`.
//! Shared canonical installation chain for CLI, panel, and hint callbacks.
use crate::utils::settings::constants::SettingSource;
use anyhow::{anyhow, bail};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
/// Maps to: CC pluginInstallationHelpers.ts:73-75#getCurrentTimestamp.
pub fn get_current_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Native argument carrier for Node path.resolve's string input. Marketplace
/// first-read JSON may be malformed even though downstream cache reads validate
/// it; preserve that exact type error at the source path consumer.
pub trait PathWithinBaseInput {
    fn to_base_path(self) -> anyhow::Result<PathBuf>;
}
impl PathWithinBaseInput for &Path {
    fn to_base_path(self) -> anyhow::Result<PathBuf> {
        Ok(self.to_owned())
    }
}
impl PathWithinBaseInput for &PathBuf {
    fn to_base_path(self) -> anyhow::Result<PathBuf> {
        Ok(self.clone())
    }
}
impl PathWithinBaseInput for Option<&Value> {
    fn to_base_path(self) -> anyhow::Result<PathBuf> {
        if let Some(Value::String(path)) = self {
            return Ok(PathBuf::from(path));
        }
        let kind = match self {
            None => "undefined",
            Some(Value::Array(_)) => "array",
            Some(Value::Bool(_)) => "boolean",
            Some(Value::Number(_)) => "number",
            _ => "object",
        };
        bail!("The \"paths[0]\" property must be of type string, got {kind}")
    }
}

/// Maps to: CC pluginInstallationHelpers.ts:87-107#validatePathWithinBase.
pub fn validate_path_within_base(
    base: impl PathWithinBaseInput,
    relative: &str,
) -> anyhow::Result<PathBuf> {
    let base = base.to_base_path()?;
    // Node path.resolve is lexical and does not normalize Unicode or follow symlinks.
    let absolute = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()?.join(base)
    };
    let normalize = |path: &Path| {
        let mut result = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    result.pop();
                }
                other => result.push(other.as_os_str()),
            }
        }
        result
    };
    let base = normalize(&absolute);
    let path = normalize(&base.join(relative));
    if path != base
        && !path.to_string_lossy().starts_with(&format!(
            "{}{sep}",
            base.display(),
            sep = std::path::MAIN_SEPARATOR
        ))
    {
        bail!("Path traversal detected: \"{relative}\" would escape the base directory");
    }
    Ok(path)
}
/// Maps to: CC pluginInstallationHelpers.ts:61-67#PluginInstallationInfo.
#[derive(Clone, Debug)]
pub struct PluginInstallationInfo {
    pub plugin_id: String,
    pub install_path: PathBuf,
    pub version: Option<String>,
}
/// Maps to: CC pluginInstallationHelpers.ts:239-256#registerPluginInstallation.
pub fn register_plugin_installation(
    info: PluginInstallationInfo,
    scope: super::schemas::PluginScope,
    project_path: Option<&str>,
) -> anyhow::Result<()> {
    let now = get_current_timestamp();
    super::installed_plugins_manager::add_installed_plugin(
        &info.plugin_id,
        super::schemas::InstalledPlugin {
            version: info
                .version
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "unknown".into()),
            installed_at: now.clone(),
            last_updated: Some(now),
            install_path: info.install_path.display().to_string(),
            git_commit_sha: None,
        },
        scope,
        project_path,
    )
}
/// Maps to: CC pluginInstallationHelpers.ts:264-276#parsePluginId.
pub fn parse_plugin_id(id: &str) -> Option<super::plugin_identifier::ParsedPluginIdentifier> {
    let parts = id.split('@').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(super::plugin_identifier::ParsedPluginIdentifier {
        name: parts[0].into(),
        marketplace: Some(parts[1].into()),
    })
}
/// Maps to: CC pluginInstallationHelpers.ts:128-226#cacheAndRegisterPlugin.
pub async fn cache_and_register_plugin(
    id: &str,
    entry: &Value,
    scope: super::schemas::PluginScope,
    project_path: Option<&str>,
    local_source: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let source = if entry["source"].is_string() {
        local_source
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| Value::String(p.display().to_string()))
            .unwrap_or_else(|| entry["source"].clone())
    } else {
        entry["source"].clone()
    };
    let fallback = serde_json::from_value::<super::schemas::PluginManifest>(entry.clone())?;
    let cached = super::plugin_loader::cache_plugin(&source, Some(&fallback)).await?;
    let sha_path = local_source
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(&cached.path);
    let sha = match cached.git_commit_sha.clone() {
        Some(sha) => Some(sha),
        None => super::installed_plugins_manager::get_git_commit_sha(sha_path).await,
    };
    let now = get_current_timestamp();
    let version = super::plugin_versioning::calculate_plugin_version(
        id,
        &entry["source"],
        Some(&cached.manifest),
        Some(sha_path),
        entry["version"].as_str(),
        cached.git_commit_sha.as_deref(),
    )
    .await;
    let versioned = super::plugin_loader::get_versioned_cache_path(id, &version);
    let mut final_path = cached.path.clone();
    if cached.path != versioned {
        tokio::fs::create_dir_all(versioned.parent().unwrap()).await?;
        super::plugin_loader::remove_path_force(&versioned).await?;
        if versioned.starts_with(&cached.path) {
            let temp = cached.path.parent().unwrap().join(format!(
                ".claude-plugin-temp-{}-{}",
                chrono::Utc::now().timestamp_millis(),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            tokio::fs::rename(&cached.path, &temp).await?;
            tokio::fs::create_dir_all(versioned.parent().unwrap()).await?;
            tokio::fs::rename(temp, &versioned).await?;
        } else {
            tokio::fs::rename(&cached.path, &versioned).await?;
        }
        final_path = versioned;
    }
    if super::zip_cache::is_plugin_zip_cache_enabled() {
        let zip = super::plugin_loader::get_versioned_zip_cache_path(id, &version);
        super::zip_cache::convert_directory_to_zip_in_place(&final_path, &zip).await?;
        final_path = zip;
    }
    super::installed_plugins_manager::add_installed_plugin(
        id,
        super::schemas::InstalledPlugin {
            version,
            installed_at: now.clone(),
            last_updated: Some(now),
            install_path: final_path.display().to_string(),
            git_commit_sha: sha,
        },
        scope,
        project_path,
    )?;
    Ok(final_path)
}
/// Maps to: CC pluginInstallationHelpers.ts:286-300#InstallCoreResult.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallCoreResult {
    Success {
        closure: Vec<String>,
        dep_note: String,
    },
    LocalSourceNoLocation {
        plugin_name: String,
    },
    SettingsWriteFailed {
        message: String,
    },
    ResolutionFailed {
        resolution: super::dependency_resolver::ResolutionResult,
    },
    BlockedByPolicy {
        plugin_name: String,
    },
    DependencyBlockedByPolicy {
        plugin_name: String,
        blocked_dependency: String,
    },
}
/// Maps to: CC pluginInstallationHelpers.ts:304-327#formatResolutionError.
pub fn format_resolution_error(result: &super::dependency_resolver::ResolutionResult) -> String {
    use super::dependency_resolver::ResolutionResult::*;
    match result {
        Cycle { chain } => format!("Dependency cycle: {}", chain.join(" → ")),
        CrossMarketplace {
            dependency,
            required_by,
        } => {
            let marketplace = super::plugin_identifier::parse_plugin_identifier(dependency)
                .marketplace
                .filter(|s| !s.is_empty());
            let location = marketplace
                .as_ref()
                .map(|m| format!("marketplace \"{m}\""))
                .unwrap_or_else(|| "a different marketplace".into());
            let hint=marketplace.map(|m|format!(" Add \"{m}\" to allowCrossMarketplaceDependenciesOn in the ROOT marketplace's marketplace.json (the marketplace of the plugin you're installing — only its allowlist applies; no transitive trust).")).unwrap_or_default();
            format!(
                "Dependency \"{dependency}\" (required by {required_by}) is in {location}, which is not in the allowlist — cross-marketplace dependencies are blocked by default. Install it manually first.{hint}"
            )
        }
        NotFound {
            missing,
            required_by,
        } => {
            let marketplace = super::plugin_identifier::parse_plugin_identifier(missing)
                .marketplace
                .filter(|s| !s.is_empty());
            match marketplace {
                Some(m) => format!(
                    "Dependency \"{missing}\" (required by {required_by}) not found. Is the \"{m}\" marketplace added?"
                ),
                None => format!(
                    "Dependency \"{missing}\" (required by {required_by}) not found in any configured marketplace"
                ),
            }
        }
        Success { .. } => unreachable!("source type excludes successful resolution"),
    }
}
/// Maps to: CC pluginInstallationHelpers.ts:348-481#installResolvedPlugin.
/// Option<Value> retains raw first-read marketplace locations until Node's
/// path consumer; narrowing them to Option<&str> would silently skip installs.
pub async fn install_resolved_plugin(
    plugin_id: &str,
    entry: &Value,
    scope: super::schemas::PluginScope,
    marketplace_install_location: Option<&Value>,
) -> anyhow::Result<InstallCoreResult> {
    use super::marketplace_manager::get_plugin_by_id;
    use InstallCoreResult::*;
    let setting_source = SettingSource::from(
        super::plugin_identifier::scope_to_setting_source(scope).map_err(anyhow::Error::msg)?,
    );
    let name = entry["name"].as_str().unwrap_or_default().to_owned();
    if super::plugin_policy::is_plugin_blocked_by_policy(plugin_id) {
        return Ok(BlockedByPolicy { plugin_name: name });
    }
    let has_location = |location: Option<&Value>| match location {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64().is_some_and(|v| v != 0.0),
        Some(_) => true,
    };
    if super::schemas::is_local_plugin_source(&entry["source"])
        && !has_location(marketplace_install_location)
    {
        return Ok(LocalSourceNoLocation { plugin_name: name });
    }
    let dep_info =
        std::sync::Mutex::new(std::collections::HashMap::<String, (Value, Option<Value>)>::new());
    if has_location(marketplace_install_location) {
        dep_info.lock().unwrap().insert(
            plugin_id.into(),
            (entry.clone(), marketplace_install_location.cloned()),
        );
    }
    let marketplace = super::plugin_identifier::parse_plugin_identifier(plugin_id).marketplace;
    let catalog = match marketplace {
        Some(marketplace) if !marketplace.is_empty() => {
            super::marketplace_manager::get_marketplace_cache_only(&marketplace).await
        }
        _ => None,
    };
    let allowed = catalog
        .as_ref()
        .and_then(|v| v["allowCrossMarketplaceDependenciesOn"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let resolution = super::dependency_resolver::resolve_dependency_closure(
        plugin_id,
        |id| {
            let dep_info = &dep_info;
            async move {
                if let Some((entry, _)) = dep_info.lock().unwrap().get(&id).cloned() {
                    return Ok(Some(entry));
                }
                if id == plugin_id {
                    return Ok(Some(entry.clone()));
                }
                let found = get_plugin_by_id(&id).await;
                if let Some(found) = &found {
                    dep_info.lock().unwrap().insert(
                        id,
                        (
                            found.entry.clone(),
                            found.marketplace_install_location.clone(),
                        ),
                    );
                }
                Ok(found.map(|found| found.entry))
            }
        },
        &super::dependency_resolver::get_enabled_plugin_ids_for_scope(setting_source),
        &allowed,
    )
    .await?;
    let super::dependency_resolver::ResolutionResult::Success { closure } = resolution else {
        return Ok(ResolutionFailed { resolution });
    };
    for id in &closure {
        if id != plugin_id && super::plugin_policy::is_plugin_blocked_by_policy(id) {
            return Ok(DependencyBlockedByPolicy {
                plugin_name: name,
                blocked_dependency: id.clone(),
            });
        }
    }
    let mut enabled = crate::utils::settings::get_settings_for_source(setting_source)
        .and_then(|s| s.enabled_plugins)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for id in &closure {
        enabled.insert(id.clone(), Value::Bool(true));
    }
    if let Err(error) = crate::utils::settings::update_settings_for_source(
        setting_source,
        &Map::from_iter([("enabledPlugins".into(), Value::Object(enabled))]),
    ) {
        return Ok(SettingsWriteFailed {
            message: error.to_string(),
        });
    }
    // Existing cwd L1 representation: MODULE_MAP.tsv rows Tool.ts/utils/cwd.ts.
    // This interactive caller has no ToolUseContext override.
    let project_path = (scope != super::schemas::PluginScope::User).then(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd())
            .display()
            .to_string()
    });
    for id in &closure {
        let mut found = dep_info.lock().unwrap().get(id).cloned();
        if found.is_none() && id == plugin_id {
            let location = get_plugin_by_id(id)
                .await
                .and_then(|info| info.marketplace_install_location);
            if has_location(location.as_ref()) {
                found = Some((entry.clone(), location));
            }
        }
        let Some((entry, location)) = found else {
            continue;
        };
        let local = if super::schemas::is_local_plugin_source(&entry["source"]) {
            Some(validate_path_within_base(
                location.as_ref(),
                entry["source"].as_str().unwrap(),
            )?)
        } else {
            None
        };
        cache_and_register_plugin(id, &entry, scope, project_path.as_deref(), local.as_deref())
            .await?;
    }
    super::cache_utils::clear_all_caches();
    let dep_note = super::dependency_resolver::format_dependency_count_suffix(
        &closure
            .iter()
            .filter(|id| id.as_str() != plugin_id)
            .cloned()
            .collect::<Vec<_>>(),
    );
    Ok(Success { closure, dep_note })
}
/// Maps to: CC pluginInstallationHelpers.ts:491-498#InstallPluginResult / InstallPluginParams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallPluginResult {
    Success { message: String },
    Failure { error: String },
}
#[derive(Clone, Debug)]
pub struct InstallPluginParams {
    pub plugin_id: String,
    pub entry: Value,
    pub marketplace_name: String,
    pub scope: Option<super::schemas::PluginScope>,
    pub trigger: Option<String>,
}
/// Maps to: CC pluginInstallationHelpers.ts:506-595#installPluginFromMarketplace.
pub async fn install_plugin_from_marketplace(params: InstallPluginParams) -> InstallPluginResult {
    let result: anyhow::Result<InstallPluginResult> = async {
        let location = super::marketplace_manager::get_plugin_by_id(&params.plugin_id)
            .await.and_then(|info| info.marketplace_install_location);
        let result = install_resolved_plugin(&params.plugin_id, &params.entry,
            params.scope.unwrap_or(super::schemas::PluginScope::User), location.as_ref()).await?;
        let dep_note = match result {
            InstallCoreResult::Success { dep_note, .. } => dep_note,
            failure => {
                let error = match failure {
                    InstallCoreResult::LocalSourceNoLocation { plugin_name } => format!("Cannot install local plugin \"{plugin_name}\" without marketplace install location"),
                    InstallCoreResult::SettingsWriteFailed { message } => format!("Failed to update settings: {message}"),
                    InstallCoreResult::ResolutionFailed { resolution } => format_resolution_error(&resolution),
                    InstallCoreResult::BlockedByPolicy { plugin_name } => format!("Plugin \"{plugin_name}\" is blocked by your organization's policy and cannot be installed"),
                    InstallCoreResult::DependencyBlockedByPolicy { plugin_name, blocked_dependency } => format!("Cannot install \"{plugin_name}\": dependency \"{blocked_dependency}\" is blocked by your organization's policy"),
                    InstallCoreResult::Success { .. } => unreachable!(),
                };
                return Ok(InstallPluginResult::Failure { error });
            }
        };
        let trigger = params.trigger.as_deref().unwrap_or("user");
        let mut metadata = serde_json::json!({
            "_PROTO_plugin_name":params.entry["name"],
            "_PROTO_marketplace_name":params.marketplace_name,
            "plugin_id":if super::plugin_identifier::is_official_marketplace_name(Some(&params.marketplace_name)) {params.plugin_id.as_str()} else {"third-party"},
            "trigger":trigger,
            "install_source":if trigger=="hint" {"ui-suggestion"} else {"ui-discover"}
        });
        if let Some(fields) = crate::utils::telemetry::plugin_telemetry::build_plugin_telemetry_fields(
            params.entry["name"].as_str().unwrap_or_default(), Some(&params.marketplace_name),
            super::managed_plugins::get_managed_plugin_names().as_ref(),
        ).as_object() { metadata.as_object_mut().unwrap().extend(fields.clone()); }
        if let Some(version) = params.entry.get("version").filter(|v| v.as_str().is_some_and(|s| !s.is_empty())) {
            metadata["version"] = version.clone();
        }
        crate::services::analytics::log_event("tengu_plugin_installed", metadata);
        Ok(InstallPluginResult::Success { message: format!("✓ Installed {}{dep_note}. Run /reload-plugins to activate.", params.entry["name"].as_str().unwrap_or_default()) })
    }.await;
    match result {
        Ok(result) => result,
        Err(error) => {
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            InstallPluginResult::Failure {
                error: format!("Failed to install: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    // Test fixture only: resolve the imported marketplace then call the real
    // UI wrapper and inspect canonical registry output, without a second DFS,
    // settings writer, cache copier, or version policy.
    async fn install_fixture(plugin_id: &str) -> anyhow::Result<PluginInstallationInfo> {
        crate::utils::settings::settings_cache::reset_settings_cache();
        let info = super::super::marketplace_manager::get_plugin_by_id(plugin_id)
            .await
            .ok_or_else(|| anyhow!("fixture plugin absent"))?;
        let marketplace = super::super::plugin_identifier::parse_plugin_identifier(plugin_id)
            .marketplace
            .unwrap();
        match install_plugin_from_marketplace(InstallPluginParams {
            plugin_id: plugin_id.into(),
            entry: info.entry,
            marketplace_name: marketplace,
            scope: Some(super::super::schemas::PluginScope::User),
            trigger: Some("hint".into()),
        })
        .await
        {
            InstallPluginResult::Failure { error } => bail!("{error}"),
            InstallPluginResult::Success { .. } => {}
        }
        let registry = super::super::installed_plugins_manager::load_installed_plugins_v2();
        let registry = registry.lock().unwrap();
        let entry = &registry["plugins"][plugin_id][0];
        Ok(PluginInstallationInfo {
            plugin_id: plugin_id.into(),
            install_path: PathBuf::from(entry["installPath"].as_str().unwrap()),
            version: entry["version"].as_str().map(str::to_owned),
        })
    }

    #[test]
    fn path_and_local_source_predicate_match_official_bun_oracle() {
        let oracle: Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-installation-review-0914/oracle.json")).unwrap();
        for case in oracle["paths"].as_array().unwrap() {
            let result = validate_path_within_base(
                Path::new(case["base"].as_str().unwrap()),
                case["path"].as_str().unwrap(),
            );
            if let Some(expected) = case["value"].as_str() {
                assert_eq!(result.unwrap(), PathBuf::from(expected));
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    case["error"].as_str().unwrap()
                );
            }
        }
        for (source, expected) in [
            ("./p", true),
            ("../p", false),
            ("p", false),
            ("/p", false),
            ("", false),
        ] {
            assert_eq!(
                super::super::schemas::is_local_plugin_source(&Value::String(source.into())),
                expected
            );
        }
    }
    #[tokio::test]
    async fn raw_location_failure_matches_official_settings_before_materialization() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("plugin-raw-location-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/known_marketplaces.json"), "{}").unwrap();
        let _config = set_config_dir(&root);
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", root.join("plugins"));
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::settings::settings_cache::reset_settings_cache();
        let entry = serde_json::json!({"name":"p","source":"./p"});
        for location in [
            None,
            Some(Value::Null),
            Some(Value::Bool(false)),
            Some(Value::String(String::new())),
        ] {
            assert_eq!(
                install_resolved_plugin(
                    "p@m",
                    &entry,
                    super::super::schemas::PluginScope::User,
                    location.as_ref()
                )
                .await
                .unwrap(),
                InstallCoreResult::LocalSourceNoLocation {
                    plugin_name: "p".into()
                }
            );
        }
        assert!(!root.join("settings.json").exists());
        let error = install_resolved_plugin(
            "p@m",
            &entry,
            super::super::schemas::PluginScope::User,
            Some(&serde_json::json!(42)),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "The \"paths[0]\" property must be of type string, got number"
        );
        let settings: Value =
            serde_json::from_slice(&std::fs::read(root.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"]["p@m"], true);
        assert!(!root.join("plugins/installed_plugins.json").exists());
        crate::utils::settings::settings_cache::reset_settings_cache();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Points `CLAUDE_CONFIG_DIR` at an isolated root and drops the caches
    /// derived from the previous one.
    fn set_config_dir(value: impl AsRef<std::ffi::OsStr>) -> EnvVarGuard {
        let env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", value);
        // Every isolated fixture starts a new process-scope registry;
        // do not reset it inside install_fixture, after the session
        // snapshot assertion has deliberately captured the old memo.
        super::super::installed_plugins_manager::clear_installed_plugins_cache();
        crate::utils::settings::settings_cache::reset_settings_cache();
        env
    }

    #[tokio::test]
    async fn canonical_install_matches_official_copies_and_registers_local_official_plugin() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-hint-install-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let marketplace = root.join("market");
        let source = marketplace.join("plugins/mail");
        std::fs::create_dir_all(source.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            source.join(".claude-plugin/plugin.json"),
            r#"{"name":"mail","version":"1.2.3"}"#,
        )
        .unwrap();
        std::fs::write(source.join("README.md"), "mail").unwrap();
        std::fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            serde_json::json!({
                "name": "claude-plugins-official", "owner": {"name": "Owner"},
                "plugins": [{"name": "mail", "source": "./plugins/mail", "version": "9.9.9"}]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({
                "claude-plugins-official": {"installLocation": marketplace}
            })
            .to_string(),
        )
        .unwrap();

        // CC installedPluginsManager.ts:488-493 + 874-912: save publishes a
        // new registry memo while retaining the session's earlier snapshot.
        crate::utils::plugins::installed_plugins_manager::clear_installed_plugins_cache();
        let session_before =
            crate::utils::plugins::installed_plugins_manager::get_in_memory_installed_plugins();
        assert_eq!(
            session_before.lock().unwrap()["plugins"],
            serde_json::json!({})
        );
        let result = install_fixture("mail@claude-plugins-official")
            .await
            .unwrap();
        assert!(result.install_path.join("README.md").is_file());
        assert_eq!(result.version.as_deref(), Some("1.2.3"));
        let current = crate::utils::plugins::installed_plugins_manager::load_installed_plugins_v2();
        assert_eq!(
            current.lock().unwrap()["plugins"]["mail@claude-plugins-official"][0]["scope"],
            "user"
        );
        let session_after =
            crate::utils::plugins::installed_plugins_manager::get_in_memory_installed_plugins();
        assert!(std::sync::Arc::ptr_eq(&session_before, &session_after));
        assert_eq!(
            session_after.lock().unwrap()["plugins"],
            serde_json::json!({})
        );
        let installed: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("plugins/installed_plugins.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            installed["plugins"]["mail@claude-plugins-official"][0]["scope"],
            "user"
        );
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["enabledPlugins"]["mail@claude-plugins-official"],
            true
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_install_matches_official_resolves_and_commits_dependency_closure() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-hint-dependencies-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", root.join("managed"));
        let marketplace = root.join("market");
        for (name, version) in [("root", "2.0.0"), ("dependency", "1.0.0")] {
            let source = marketplace.join(format!("plugins/{name}/.claude-plugin"));
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(
                source.join("plugin.json"),
                serde_json::json!({"name": name, "version": version}).to_string(),
            )
            .unwrap();
            std::fs::write(source.parent().unwrap().join("README.md"), name).unwrap();
        }
        std::fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            serde_json::json!({
                "name": "claude-plugins-official", "owner": {"name": "Owner"},
                "plugins": [
                    {"name": "root", "source": "./plugins/root", "dependencies": ["dependency"]},
                    {"name": "dependency", "source": "./plugins/dependency"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({
                "claude-plugins-official": {"installLocation": marketplace}
            })
            .to_string(),
        )
        .unwrap();

        install_fixture("root@claude-plugins-official")
            .await
            .unwrap();
        let installed: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("plugins/installed_plugins.json")).unwrap(),
        )
        .unwrap();
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("settings.json")).unwrap())
                .unwrap();
        for id in [
            "root@claude-plugins-official",
            "dependency@claude-plugins-official",
        ] {
            assert!(installed["plugins"][id].is_array(), "{id}");
            assert_eq!(settings["enabledPlugins"][id], true, "{id}");
        }
        assert!(
            root.join("plugins/cache/claude-plugins-official/dependency/1.0.0/README.md")
                .is_file()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_settings_failure_precedes_materialization_matches_official_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-hint-rollback-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let marketplace = root.join("market");
        std::fs::create_dir_all(marketplace.join("plugins/root/.claude-plugin")).unwrap();
        std::fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            marketplace.join("plugins/root/.claude-plugin/plugin.json"),
            r#"{"name":"root","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            r#"{"name":"claude-plugins-official","owner":{"name":"Owner"},"plugins":[{"name":"root","source":"./plugins/root"}]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({
                "claude-plugins-official": {"installLocation": marketplace}
            })
            .to_string(),
        )
        .unwrap();
        // CC pluginInstallationHelpers.ts:427-442: failed settings aborts before materialization.
        // A directory creates a real I/O error; [] is normalized by the canonical settings writer.
        std::fs::create_dir(root.join("settings.json")).unwrap();

        assert!(
            install_fixture("root@claude-plugins-official")
                .await
                .unwrap_err()
                .to_string()
                .contains("Failed to update settings:")
        );
        assert!(
            !root
                .join("plugins/cache/claude-plugins-official/root/1.0.0")
                .exists()
        );
        assert!(!root.join("plugins/installed_plugins.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_install_matches_official_rejects_corrupt_source_manifest() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-hint-corrupt-manifest-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let marketplace = root.join("market");
        std::fs::create_dir_all(marketplace.join("plugins/root/.claude-plugin")).unwrap();
        std::fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            marketplace.join("plugins/root/.claude-plugin/plugin.json"),
            "{not-json",
        )
        .unwrap();
        std::fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            r#"{"name":"claude-plugins-official","owner":{"name":"Owner"},"plugins":[{"name":"root","source":"./plugins/root"}]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({
                "claude-plugins-official": {"installLocation": marketplace}
            })
            .to_string(),
        )
        .unwrap();

        assert!(
            install_fixture("root@claude-plugins-official")
                .await
                .unwrap_err()
                .to_string()
                .contains("corrupt manifest")
        );
        // CC pluginInstallationHelpers.ts:427-477: cache failure does not roll settings back.
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["enabledPlugins"]["root@claude-plugins-official"],
            true
        );
        assert!(!root.join("plugins/installed_plugins.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn order_fixture(root: &Path, missing_root: bool) {
        let market = root.join("market");
        std::fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        for name in ["root", "dep"] {
            if missing_root && name == "root" {
                continue;
            }
            let source = market.join(name);
            std::fs::create_dir_all(source.join(".claude-plugin")).unwrap();
            std::fs::write(
                source.join(".claude-plugin/plugin.json"),
                serde_json::json!({"name":name,"version":"1.0"}).to_string(),
            )
            .unwrap();
            std::fs::write(source.join("README.md"), name).unwrap();
        }
        std::fs::write(
            market.join(".claude-plugin/marketplace.json"),
            serde_json::json!({"name":"claude-plugins-official","owner":{"name":"Owner"},"plugins":[
                {"name":"root","source":"./root","dependencies":["dep"]},
                {"name":"dep","source":"./dep"}
            ]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({"claude-plugins-official":{"installLocation":market}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"enabledPlugins":{"existing":false},"customSetting":7}"#,
        )
        .unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::utils::plugins::installed_plugins_manager::clear_installed_plugins_cache();
    }

    #[tokio::test]
    async fn canonical_second_member_failure_keeps_prior_effects_matches_official_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-plugin-order-{}", uuid::Uuid::new_v4()));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        order_fixture(&root, true);
        let error = install_fixture("root@claude-plugins-official")
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty()); // Native filesystem ENOENT after dependency commit.
        // CC pluginInstallationHelpers.ts:427-477, actual AST-extracted Bun oracle
        // proof/plugin-installed-write-0914/bun-order-oracle.json: second cache failure
        // retains the whole enabled closure and the first member's registration.
        let settings: Value =
            serde_json::from_slice(&std::fs::read(root.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            settings["enabledPlugins"],
            serde_json::json!({
                "existing":false,"dep@claude-plugins-official":true,"root@claude-plugins-official":true
            })
        );
        assert_eq!(settings["customSetting"], 7);
        let registry: Value = serde_json::from_slice(
            &std::fs::read(root.join("plugins/installed_plugins.json")).unwrap(),
        )
        .unwrap();
        assert!(
            registry["plugins"]
                .get("root@claude-plugins-official")
                .is_none()
        );
        assert_eq!(
            registry["plugins"]["dep@claude-plugins-official"][0]["scope"],
            "user"
        );
        assert!(
            root.join("plugins/cache/claude-plugins-official/dep/1.0/README.md")
                .is_file()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_registry_failure_keeps_materialization_matches_official_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-save-order-{}",
            uuid::Uuid::new_v4()
        ));
        let _config = set_config_dir(&root);
        let _home = EnvVarGuard::set(HOME_VAR, &root);
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        order_fixture(&root, false);
        std::fs::create_dir(root.join("plugins/installed_plugins.json")).unwrap();
        assert!(
            install_fixture("root@claude-plugins-official")
                .await
                .is_err()
        );
        // CC cacheAndRegisterPlugin:212-223 throws after caching if registry save fails;
        // installResolvedPlugin:446-477 has no rollback and no final cache clear on throw.
        assert!(
            root.join("plugins/cache/claude-plugins-official/dep/1.0/README.md")
                .is_file()
        );
        assert!(
            !root
                .join("plugins/cache/claude-plugins-official/root/1.0")
                .exists()
        );
        let settings: Value =
            serde_json::from_slice(&std::fs::read(root.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            settings["enabledPlugins"]["dep@claude-plugins-official"],
            true
        );
        assert_eq!(
            settings["enabledPlugins"]["root@claude-plugins-official"],
            true
        );
        assert!(root.join("plugins/installed_plugins.json").is_dir());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn canonical_root_catalog_allowlist_matches_official_no_transitive_trust() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("plugin-root-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        crate::utils::settings::settings_cache::set_cached_settings_for_source(
            SettingSource::Policy,
            None,
        );
        let catalog = |name: &str, dependencies: Value, allowed: Value| {
            serde_json::json!({
                "name":name, "owner":{"name":"Owner"},
                "plugins":[{"name":"p","source":"./p","dependencies":dependencies}],
                "allowCrossMarketplaceDependenciesOn":allowed
            })
        };
        let mut config = serde_json::Map::new();
        for (name, dependencies, allowed) in [
            ("a", serde_json::json!(["p@b"]), serde_json::json!(["b"])),
            ("b", serde_json::json!(["p@c"]), serde_json::json!(["c"])),
            ("c", serde_json::json!([]), serde_json::json!([])),
        ] {
            let path = root.join(format!("{name}.json"));
            std::fs::write(&path, catalog(name, dependencies, allowed).to_string()).unwrap();
            config.insert(name.into(), serde_json::json!({"installLocation":path}));
        }
        std::fs::write(
            root.join("known_marketplaces.json"),
            Value::Object(config).to_string(),
        )
        .unwrap();
        // CC installResolvedPlugin:391-397 + dependencyResolver:126-139: only
        // root A's catalog list applies, even though B independently trusts C.
        let entry = catalog("a", serde_json::json!(["p@b"]), serde_json::json!(["b"]))["plugins"]
            [0]
        .clone();
        let location = Value::String(root.display().to_string());
        let result = install_resolved_plugin(
            "p@a",
            &entry,
            super::super::schemas::PluginScope::User,
            Some(&location),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, InstallCoreResult::ResolutionFailed { resolution: super::super::dependency_resolver::ResolutionResult::CrossMarketplace { ref dependency, .. } } if dependency == "p@c")
        );
        assert!(!root.join("installed_plugins.json").exists());
        std::fs::remove_dir_all(root).unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    #[test]
    fn raw_location_path_error_matches_official_bun_argument_type() {
        // CC installResolvedPlugin:458-463 -> validatePathWithinBase:90;
        // actual Bun path.resolve oracle is recorded in the policy report.
        for (location, kind) in [
            (None, "undefined"),
            (Some(serde_json::json!(null)), "object"),
            (Some(serde_json::json!(42)), "number"),
            (Some(serde_json::json!({})), "object"),
            (Some(serde_json::json!([])), "array"),
            (Some(serde_json::json!(false)), "boolean"),
        ] {
            let original = location.clone();
            let error = validate_path_within_base(location.as_ref(), "./p").unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("The \"paths[0]\" property must be of type string, got {kind}")
            );
            assert_eq!(location, original);
        }
    }
}
