//! Maps to: CC `utils/plugins/installedPluginsManager.ts`.
//! Canonical registry read/write and session-snapshot slice. Startup migration,
//! pending-update UI and uninstall consumers are not yet ported by this file.

use super::schemas::{InstalledPlugin, PluginScope};
use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
use crate::utils::log::{LogError, log_error};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

/// Maps to: CC installedPluginsManager.ts:41#InstalledPluginsMapV2.
type InstalledPluginsMapV2 = serde_json::Map<String, Value>;
/// Maps to: CC installedPluginsManager.ts:44#PersistableScope.
pub type PersistableScope = PluginScope;
/// Necessary mutable JavaScript object-reference carrier: getters share one
/// object, while save replaces the memo without replacing the session snapshot.
/// Canonical schema output stays as an ordered object: typed projection would
/// erase construction-vs-parse field order when serializing a newly added entry.
pub type InstalledPluginsReference = Arc<Mutex<Value>>;

/// Maps to: CC installedPluginsManager.ts:64,71 module state.
static INSTALLED_PLUGINS_CACHE_V2: LazyLock<Mutex<Option<InstalledPluginsReference>>> =
    LazyLock::new(|| Mutex::new(None));
static IN_MEMORY_INSTALLED_PLUGINS: LazyLock<Mutex<Option<InstalledPluginsReference>>> =
    LazyLock::new(|| Mutex::new(None));

// Native synchronization carrier, following the existing log/memoize synchronous
// turn adapters. A JS synchronous read-modify-write cannot interleave with a
// second installation; nested source function calls must not deadlock.
static INSTALLED_PLUGINS_TURN: Mutex<()> = Mutex::new(());
thread_local! { static IN_INSTALLED_PLUGINS_TURN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
struct InstalledPluginsTurn {
    guard: Option<MutexGuard<'static, ()>>,
}
impl InstalledPluginsTurn {
    fn enter() -> Self {
        if IN_INSTALLED_PLUGINS_TURN.with(std::cell::Cell::get) {
            return Self { guard: None };
        }
        let guard = INSTALLED_PLUGINS_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        IN_INSTALLED_PLUGINS_TURN.with(|entered| entered.set(true));
        Self { guard: Some(guard) }
    }
}
impl Drop for InstalledPluginsTurn {
    fn drop(&mut self) {
        if self.guard.is_some() {
            IN_INSTALLED_PLUGINS_TURN.with(|entered| entered.set(false));
        }
    }
}

/// Maps to: CC installedPluginsManager.ts:78-80#getInstalledPluginsFilePath.
pub fn get_installed_plugins_file_path() -> PathBuf {
    let joined = super::plugin_directories::get_plugins_directory().join("installed_plugins.json");
    // Node join is lexical; it neither resolves symlinks nor normalizes Unicode.
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if result.file_name().is_some_and(|name| name != "..") {
                    result.pop();
                } else if !result.has_root() {
                    result.push("..");
                }
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}

/// Maps to: CC installedPluginsManager.ts:99-103#clearInstalledPluginsCache.
pub fn clear_installed_plugins_cache() {
    let _turn = InstalledPluginsTurn::enter();
    *INSTALLED_PLUGINS_CACHE_V2.lock().unwrap() = None;
    *IN_MEMORY_INSTALLED_PLUGINS.lock().unwrap() = None;
    log_for_debugging("Cleared installed plugins cache");
}

/// Maps to: CC installedPluginsManager.ts:259-278#readInstalledPluginsFileRaw.
/// Native LogError retains exception names at the shared logging boundary.
/// The pre-existing JSON->Value nonfinite/lone UTF-16 limits still apply.
fn read_installed_plugins_file_raw() -> Result<Option<(f64, Value)>, LogError> {
    let _turn = InstalledPluginsTurn::enter();
    let path = get_installed_plugins_file_path();
    let content = match crate::utils::fs_operations::get_fs_implementation()
        .read_file_sync(&path, crate::utils::fs_operations::BufferEncoding::Utf8)
        .map(|text| text.to_string_lossy().into_bytes())
    {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LogError::new(
                crate::utils::errors::format_native_file_error(
                    &error,
                    if error.kind() == std::io::ErrorKind::IsADirectory {
                        "read"
                    } else {
                        "open"
                    },
                    (error.kind() != std::io::ErrorKind::IsADirectory).then_some(path.as_path()),
                ),
            ));
        }
    };
    let data = crate::utils::slow_operations::json_parse(&String::from_utf8_lossy(&content))
        .map_err(|error| {
            let mut result = LogError::new(error.to_string());
            result.name = "SyntaxError".into();
            result.stack = result
                .stack
                .map(|stack| stack.replacen("Error:", "SyntaxError:", 1));
            result
        })?
        .to_json();
    let version = data.get("version").and_then(Value::as_f64).unwrap_or(1.0);
    Ok(Some((version, data)))
}

/// Maps to: CC installedPluginsManager.ts:284-305#migrateV1ToV2.
fn migrate_v1_to_v2(v1_data: Value) -> Value {
    let mut plugins = InstalledPluginsMapV2::new();
    for (plugin_id, plugin) in v1_data["plugins"].as_object().expect("validated V1 record") {
        let install_path = super::plugin_loader::get_versioned_cache_path(
            plugin_id,
            plugin["version"].as_str().expect("validated V1 version"),
        );
        let mut entry = serde_json::Map::new();
        entry.insert("scope".into(), Value::String("user".into()));
        entry.insert(
            "installPath".into(),
            Value::String(install_path.to_string_lossy().into_owned()),
        );
        entry.insert("version".into(), plugin["version"].clone());
        entry.insert("installedAt".into(), plugin["installedAt"].clone());
        for key in ["lastUpdated", "gitCommitSha"] {
            if let Some(value) = plugin.get(key) {
                entry.insert(key.into(), value.clone());
            }
        }
        plugins.insert(plugin_id.clone(), Value::Array(vec![Value::Object(entry)]));
    }
    serde_json::json!({"version": 2, "plugins": plugins})
}

/// Maps to: CC installedPluginsManager.ts:315-364#loadInstalledPluginsV2.
pub fn load_installed_plugins_v2() -> InstalledPluginsReference {
    let _turn = InstalledPluginsTurn::enter();
    if let Some(data) = INSTALLED_PLUGINS_CACHE_V2.lock().unwrap().as_ref() {
        return data.clone();
    }
    let file_path = get_installed_plugins_file_path();
    let loaded = (|| -> Result<Value, LogError> {
        if let Some((version, data)) = read_installed_plugins_file_raw()? {
            let schema = if version == 2.0 {
                super::schemas::installed_plugins_file_schema_v2()
            } else {
                super::schemas::installed_plugins_file_schema_v1()
            };
            let validated = crate::utils::zod::safe_parse(schema, &data).map_err(|error| {
                let mut result = LogError::new(error.message());
                result.name = "ZodError".into();
                result.stack = result
                    .stack
                    .map(|stack| stack.replacen("Error:", "ZodError:", 1));
                result
            })?;
            if version == 2.0 {
                log_for_debugging(&format!(
                    "Loaded {} installed plugins from {}",
                    validated["plugins"].as_object().unwrap().len(),
                    file_path.display()
                ));
                return Ok(validated);
            }
            let count = validated["plugins"].as_object().unwrap().len();
            let v2 = migrate_v1_to_v2(validated);
            log_for_debugging(&format!(
                "Loaded and converted {count} plugins from V1 format"
            ));
            return Ok(v2);
        }
        log_for_debugging("installed_plugins.json doesn't exist, returning empty V2 object");
        Ok(serde_json::json!({"version": 2, "plugins": {}}))
    })();
    let data = match loaded {
        Ok(data) => data,
        Err(error) => {
            log_for_debugging_with_level(
                &format!(
                    "Failed to load installed_plugins.json: {}. Starting with empty state.",
                    error.message
                ),
                DebugLogLevel::Error,
            );
            log_error(error);
            serde_json::json!({"version": 2, "plugins": {}})
        }
    };
    let data = Arc::new(Mutex::new(data));
    *INSTALLED_PLUGINS_CACHE_V2.lock().unwrap() = Some(data.clone());
    data
}

/// Maps to: CC installedPluginsManager.ts:370-394#saveInstalledPluginsV2.
fn save_installed_plugins_v2(data: InstalledPluginsReference) -> anyhow::Result<()> {
    let _turn = InstalledPluginsTurn::enter();
    let path = get_installed_plugins_file_path();
    let result = (|| -> anyhow::Result<()> {
        // NodeFsOperations.mkdirSync is recursive (fsOperations.ts).
        crate::utils::fs_operations::get_fs_implementation()
            .mkdir_sync(&super::plugin_directories::get_plugins_directory(), None)?;
        let json = data.lock().unwrap().clone();
        crate::utils::slow_operations::write_file_sync_deprecated(
            &path,
            &crate::utils::slow_operations::json_stringify(&json, 2),
            true,
        )?;
        *INSTALLED_PLUGINS_CACHE_V2.lock().unwrap() = Some(data.clone());
        log_for_debugging(&format!(
            "Saved {} installed plugins to {}",
            data.lock().unwrap()["plugins"]
                .as_object()
                .expect("V2 record")
                .len(),
            path.display()
        ));
        Ok(())
    })();
    if let Err(error) = &result {
        log_error(LogError::new(error.to_string()));
    }
    result
}

/// Maps to: CC installedPluginsManager.ts:488-493#getInMemoryInstalledPlugins.
pub fn get_in_memory_installed_plugins() -> InstalledPluginsReference {
    let _turn = InstalledPluginsTurn::enter();
    if let Some(data) = IN_MEMORY_INSTALLED_PLUGINS.lock().unwrap().as_ref() {
        return data.clone();
    }
    let data = load_installed_plugins_v2();
    *IN_MEMORY_INSTALLED_PLUGINS.lock().unwrap() = Some(data.clone());
    data
}

/// Maps to: CC installedPluginsManager.ts:502-524#loadInstalledPluginsFromDisk.
pub fn load_installed_plugins_from_disk() -> Value {
    let _turn = InstalledPluginsTurn::enter();
    let loaded = (|| -> Result<Value, LogError> {
        if let Some((version, data)) = read_installed_plugins_file_raw()? {
            let schema = if version == 2.0 {
                super::schemas::installed_plugins_file_schema_v2()
            } else {
                super::schemas::installed_plugins_file_schema_v1()
            };
            let validated = crate::utils::zod::safe_parse(schema, &data).map_err(|error| {
                let mut result = LogError::new(error.message());
                result.name = "ZodError".into();
                result.stack = result
                    .stack
                    .map(|stack| stack.replacen("Error:", "ZodError:", 1));
                result
            })?;
            if version == 2.0 {
                return Ok(validated);
            }
            return Ok(migrate_v1_to_v2(validated));
        }
        Ok(serde_json::json!({"version": 2, "plugins": {}}))
    })();
    match loaded {
        Ok(data) => data,
        Err(error) => {
            log_for_debugging_with_level(
                &format!(
                    "Failed to load installed plugins from disk: {}",
                    error.message
                ),
                DebugLogLevel::Error,
            );
            serde_json::json!({"version": 2, "plugins": {}})
        }
    }
}

/// Maps to: CC installedPluginsManager.ts:702-704#resetInMemoryState.
pub fn reset_in_memory_state() {
    let _turn = InstalledPluginsTurn::enter();
    *IN_MEMORY_INSTALLED_PLUGINS.lock().unwrap() = None;
}

/// Maps to: CC installedPluginsManager.ts:800-808#isInstallationRelevantToCurrentProject.
pub fn is_installation_relevant_to_current_project(inst: &Value) -> bool {
    matches!(
        inst.get("scope").and_then(Value::as_str),
        Some("user" | "managed")
    ) || inst.get("projectPath").and_then(Value::as_str)
        == Some(
            crate::bootstrap::state::get_original_cwd()
                .to_string_lossy()
                .as_ref(),
        )
}

/// Maps to: CC installedPluginsManager.ts:818-831#isPluginInstalled.
pub fn is_plugin_installed(plugin_id: &str) -> bool {
    let _turn = InstalledPluginsTurn::enter();
    let data = load_installed_plugins_v2();
    let data = data.lock().unwrap();
    let Some(installations) = data["plugins"].get(plugin_id).and_then(Value::as_array) else {
        return false;
    };
    if installations.is_empty() {
        return false;
    }
    if !installations
        .iter()
        .any(is_installation_relevant_to_current_project)
    {
        return false;
    }
    // getSettings_DEPRECATED is the getInitialSettings alias, settings.ts:820.
    crate::utils::settings::get_initial_settings()
        .enabled_plugins
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|plugins| plugins.contains_key(plugin_id))
}

/// Maps to: CC installedPluginsManager.ts:849-862#isPluginGloballyInstalled.
pub fn is_plugin_globally_installed(plugin_id: &str) -> bool {
    let _turn = InstalledPluginsTurn::enter();
    let data = load_installed_plugins_v2();
    let data = data.lock().unwrap();
    let Some(installations) = data["plugins"].get(plugin_id).and_then(Value::as_array) else {
        return false;
    };
    if installations.is_empty() {
        return false;
    }
    let global = installations.iter().any(|entry| {
        matches!(
            entry.get("scope").and_then(Value::as_str),
            Some("user" | "managed")
        )
    });
    if !global {
        return false;
    }
    crate::utils::settings::get_initial_settings()
        .enabled_plugins
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|plugins| plugins.contains_key(plugin_id))
}

/// Maps to: CC installedPluginsManager.ts:874-912#addInstalledPlugin.
/// Caller explicitly supplies the source default User scope when omitted in TS.
pub fn add_installed_plugin(
    plugin_id: &str,
    metadata: InstalledPlugin,
    scope: PersistableScope,
    project_path: Option<&str>,
) -> anyhow::Result<()> {
    let _turn = InstalledPluginsTurn::enter();
    let mut data = load_installed_plugins_from_disk();
    let scope = match scope {
        PluginScope::Managed => "managed",
        PluginScope::User => "user",
        PluginScope::Project => "project",
        PluginScope::Local => "local",
    };
    // Preserve the construction order in the source, including projectPath
    // last. The schema's parsed order is different; do not serialize a typed
    // struct over the source-owned mutable object on this write path.
    let mut entry = serde_json::Map::new();
    entry.insert("scope".into(), Value::String(scope.into()));
    entry.insert("installPath".into(), Value::String(metadata.install_path));
    entry.insert("version".into(), Value::String(metadata.version));
    entry.insert("installedAt".into(), Value::String(metadata.installed_at));
    if let Some(value) = metadata.last_updated {
        entry.insert("lastUpdated".into(), Value::String(value));
    }
    if let Some(value) = metadata.git_commit_sha {
        entry.insert("gitCommitSha".into(), Value::String(value));
    }
    if let Some(path) = project_path.filter(|path| !path.is_empty()) {
        entry.insert("projectPath".into(), Value::String(path.into()));
    }
    let plugins = data["plugins"]
        .as_object_mut()
        .expect("validated V2 plugins");
    let installations = plugins
        .entry(plugin_id.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("validated installation array");
    let index = installations.iter().position(|entry| {
        entry.get("scope").and_then(Value::as_str) == Some(scope)
            && entry.get("projectPath").and_then(Value::as_str) == project_path
    });
    let is_update = index.is_some();
    if let Some(index) = index {
        installations[index] = Value::Object(entry);
    } else {
        installations.push(Value::Object(entry));
    }
    save_installed_plugins_v2(Arc::new(Mutex::new(data)))?;
    log_for_debugging(&format!(
        "{} installed plugin: {plugin_id} (scope: {scope})",
        if is_update { "Updated" } else { "Added" }
    ));
    Ok(())
}

/// Maps to: CC installedPluginsManager.ts:1002-1005#getGitCommitSha.
pub async fn get_git_commit_sha(dir_path: &Path) -> Option<String> {
    crate::utils::git::git_filesystem::get_head_for_dir(dir_path).await
}

/// Maps to: CC installedPluginsManager.ts:746-789 return object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoveAllPluginsForMarketplaceResult {
    pub orphaned_paths: Vec<String>,
    pub removed_plugin_ids: Vec<String>,
}

/// Maps to: CC installedPluginsManager.ts:746-789#removeAllPluginsForMarketplace.
pub fn remove_all_plugins_for_marketplace(
    marketplace_name: &str,
) -> anyhow::Result<RemoveAllPluginsForMarketplaceResult> {
    let _turn = InstalledPluginsTurn::enter();
    let mut result = RemoveAllPluginsForMarketplaceResult::default();
    if marketplace_name.is_empty() {
        return Ok(result);
    }
    let mut data = load_installed_plugins_from_disk();
    let suffix = format!("@{marketplace_name}");
    let plugins = data["plugins"]
        .as_object_mut()
        .expect("validated V2 plugins");
    let ids: Vec<_> = plugins
        .keys()
        .filter(|id| id.ends_with(&suffix))
        .cloned()
        .collect();
    for id in ids {
        for entry in plugins[&id].as_array().expect("validated installations") {
            if let Some(path) = entry
                .get("installPath")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
            {
                if !result
                    .orphaned_paths
                    .iter()
                    .any(|existing| existing == path)
                {
                    result.orphaned_paths.push(path.into());
                }
            }
        }
        plugins.remove(&id);
        log_for_debugging(&format!(
            "Removed installed plugin for marketplace removal: {id}"
        ));
        result.removed_plugin_ids.push(id);
    }
    if !result.removed_plugin_ids.is_empty() {
        save_installed_plugins_v2(Arc::new(Mutex::new(data)))?;
    }
    Ok(result)
}

/// Maps to: CC installedPluginsManager.ts:453-476#removePluginInstallation.
pub fn remove_plugin_installation(
    plugin_id: &str,
    scope: PersistableScope,
    project_path: Option<&str>,
) -> anyhow::Result<()> {
    let _turn = InstalledPluginsTurn::enter();
    let mut data = load_installed_plugins_from_disk();
    let scope = serde_json::to_value(scope)?;
    let plugins = data["plugins"]
        .as_object_mut()
        .expect("validated V2 plugins");
    let Some(installations) = plugins.get_mut(plugin_id).and_then(Value::as_array_mut) else {
        return Ok(());
    };
    installations.retain(|entry| {
        !(entry.get("scope") == Some(&scope)
            && entry.get("projectPath").and_then(Value::as_str) == project_path)
    });
    if installations.is_empty() {
        plugins.remove(plugin_id);
    }
    save_installed_plugins_v2(Arc::new(Mutex::new(data)))?;
    log_for_debugging(&format!(
        "Removed installation for {plugin_id} at scope {}",
        scope.as_str().unwrap()
    ));
    Ok(())
}

/// Maps to: CC installedPluginsManager.ts:533-586#updateInstallationPathOnDisk.
pub fn update_installation_path_on_disk(
    plugin_id: &str,
    scope: PersistableScope,
    project_path: Option<&str>,
    new_path: &str,
    new_version: &str,
    git_commit_sha: Option<&str>,
) -> anyhow::Result<()> {
    let _turn = InstalledPluginsTurn::enter();
    let mut data = load_installed_plugins_from_disk();
    let Some(installations) = data["plugins"]
        .get_mut(plugin_id)
        .and_then(Value::as_array_mut)
    else {
        log_for_debugging(&format!(
            "Cannot update {plugin_id} on disk: plugin not found in installed plugins"
        ));
        return Ok(());
    };
    let scope = serde_json::to_value(scope)?;
    let Some(entry) = installations.iter_mut().find(|entry| {
        entry.get("scope") == Some(&scope)
            && entry.get("projectPath").and_then(Value::as_str) == project_path
    }) else {
        log_for_debugging(&format!(
            "Cannot update {plugin_id} on disk: no installation for scope {}",
            scope.as_str().unwrap()
        ));
        return Ok(());
    };
    entry["installPath"] = Value::String(new_path.into());
    entry["version"] = Value::String(new_version.into());
    entry["lastUpdated"] =
        Value::String(super::plugin_installation_helpers::get_current_timestamp());
    if let Some(sha) = git_commit_sha {
        entry["gitCommitSha"] = Value::String(sha.into());
    }
    crate::utils::slow_operations::write_file_sync_deprecated(
        &get_installed_plugins_file_path(),
        &crate::utils::slow_operations::json_stringify(&data, 2),
        true,
    )?;
    *INSTALLED_PLUGINS_CACHE_V2.lock().unwrap() = None;
    log_for_debugging(&format!(
        "Updated {plugin_id} on disk to version {new_version} at {new_path}"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::json;

    fn oracle() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-installed-0914/bun-oracle.json"
        ))
        .unwrap()
    }
    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("cometix-installed-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn write(value: &Value) {
        std::fs::write(
            get_installed_plugins_file_path(),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }
    fn metadata() -> InstalledPlugin {
        InstalledPlugin {
            version: "new".into(),
            installed_at: "T".into(),
            last_updated: None,
            install_path: "/new".into(),
            git_commit_sha: None,
        }
    }
    fn settings(value: Value) {
        crate::utils::settings::settings_cache::set_session_settings_cache(
            crate::utils::settings::validation::SettingsWithErrors {
                settings: crate::utils::settings::SettingsJson {
                    enabled_plugins: Some(value),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }

    #[test]
    fn installed_registry_reads_and_reference_identity_match_official_bun() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("read");
        for case in oracle()["readCases"].as_array().unwrap() {
            let dir = root.join(case["name"].as_str().unwrap());
            std::fs::create_dir(&dir).unwrap();
            let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &dir);
            clear_installed_plugins_cache();
            if case["missing"] != true {
                let input = &case["input"];
                if let Some(raw) = input.as_str() {
                    std::fs::write(get_installed_plugins_file_path(), raw).unwrap();
                } else {
                    write(input);
                }
            }
            let before = std::fs::read(get_installed_plugins_file_path()).ok();
            let data = load_installed_plugins_v2();
            // Original functions run in proof/plugin-installed-0914/oracle.ts;
            // CC :315-364 memoizes even missing/schema-error empty results.
            let normalized = serde_json::to_string(&*data.lock().unwrap())
                .unwrap()
                .replace(root.to_str().unwrap(), "$ROOT");
            assert_eq!(
                serde_json::from_str::<Value>(&normalized).unwrap(),
                case["value"],
                "{}",
                case["name"]
            );
            assert!(Arc::ptr_eq(&data, &load_installed_plugins_v2()));
            assert!(Arc::ptr_eq(&data, &get_in_memory_installed_plugins()));
            assert_eq!(
                std::fs::read(get_installed_plugins_file_path()).ok(),
                before
            );
            // Fresh disk reads bypass a memoized failure as well as success.
            write(
                &json!({"version":2,"plugins":{"later@m":[{"scope":"user","installPath":"/later"}]}}),
            );
            assert!(
                load_installed_plugins_from_disk()["plugins"]
                    .get("later@m")
                    .is_some()
            );
            assert!(Arc::ptr_eq(&data, &load_installed_plugins_v2()));
        }
        clear_installed_plugins_cache();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_registry_save_snapshot_and_raw_order_match_official_bun() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("lifecycle");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_installed_plugins_cache();
        let initial = get_in_memory_installed_plugins();
        write(
            &json!({"version":2,"plugins":{"external@m":[{"scope":"user","installPath":"/external"}]}}),
        );
        let cache_before = load_installed_plugins_v2().lock().unwrap().clone();
        let memory_before = initial.lock().unwrap().clone();
        let before = json!({"cache":cache_before,"disk":load_installed_plugins_from_disk(),"memory":memory_before});
        assert_eq!(before, oracle()["lifecycle"]["beforeAdd"]);
        add_installed_plugin(
            "p@m",
            InstalledPlugin {
                version: "1".into(),
                installed_at: "T".into(),
                last_updated: Some("U".into()),
                install_path: "/new".into(),
                git_commit_sha: Some("sha".into()),
            },
            PluginScope::Project,
            Some("/current"),
        )
        .unwrap();
        let saved = load_installed_plugins_v2();
        // CC :370-394/:488-493: replace memo, retain old session object.
        assert!(!Arc::ptr_eq(&saved, &initial));
        assert!(Arc::ptr_eq(&initial, &get_in_memory_installed_plugins()));
        assert_eq!(
            *saved.lock().unwrap(),
            oracle()["lifecycle"]["afterAdd"]["cache"]
        );
        assert_eq!(
            std::fs::read_to_string(get_installed_plugins_file_path()).unwrap(),
            oracle()["lifecycle"]["afterAdd"]["raw"].as_str().unwrap()
        );
        // No deep-copy getter: source references observe mutations.
        saved.lock().unwrap()["plugins"]["p@m"][0]["version"] = json!("mutated");
        assert_eq!(
            load_installed_plugins_v2().lock().unwrap()["plugins"]["p@m"][0]["version"],
            "mutated"
        );
        assert_eq!(
            load_installed_plugins_from_disk()["plugins"]["p@m"][0]["version"],
            "1"
        );
        reset_in_memory_state();
        assert!(Arc::ptr_eq(&saved, &get_in_memory_installed_plugins()));
        clear_installed_plugins_cache();
        assert!(!Arc::ptr_eq(&saved, &load_installed_plugins_v2()));
        std::fs::remove_dir_all(root).unwrap();
        clear_installed_plugins_cache();
    }

    #[test]
    fn installed_registry_scope_replacement_and_failure_cache_match_official_bun() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("scope");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_installed_plugins_cache();
        write(
            &json!({"version":2,"plugins":{"p@m":[{"scope":"user","installPath":"/old","version":"old"},{"scope":"project","projectPath":"/other","installPath":"/other"}]}}),
        );
        // CC :896-900 matches undefined differently from empty projectPath;
        // :889 omits a falsy new projectPath, so repeated "" appends again.
        for project in [None, Some(""), Some("")] {
            add_installed_plugin("p@m", metadata(), PluginScope::User, project).unwrap();
        }
        assert_eq!(
            *load_installed_plugins_v2().lock().unwrap(),
            oracle()["scope"]["data"]
        );
        assert_eq!(
            std::fs::read_to_string(get_installed_plugins_file_path()).unwrap(),
            oracle()["scope"]["raw"].as_str().unwrap()
        );
        let cached = load_installed_plugins_v2();
        let memory = get_in_memory_installed_plugins();
        std::fs::remove_file(get_installed_plugins_file_path()).unwrap();
        std::fs::create_dir(get_installed_plugins_file_path()).unwrap();
        assert!(add_installed_plugin("new@m", metadata(), PluginScope::User, None).is_err());
        // Source cache assignment occurs only after successful write/flush.
        assert!(Arc::ptr_eq(&cached, &load_installed_plugins_v2()));
        assert!(Arc::ptr_eq(&memory, &get_in_memory_installed_plugins()));
        assert!(
            load_installed_plugins_from_disk()["plugins"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
        clear_installed_plugins_cache();
    }

    #[test]
    fn installed_registry_project_and_disabled_settings_match_official_bun() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("predicate");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_installed_plugins_cache();
        let previous = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd("/current");
        let mut plugins = serde_json::Map::new();
        for (name, scope, path) in [
            ("user", "user", None),
            ("managed", "managed", None),
            ("here", "project", Some("/current")),
            ("elsewhere", "project", Some("/other")),
            ("local", "local", Some("/current")),
            ("absent", "user", None),
        ] {
            let mut entry = json!({"scope":scope,"installPath":"/x"});
            if let Some(path) = path {
                entry["projectPath"] = json!(path);
            }
            plugins.insert(format!("{name}@m"), json!([entry]));
        }
        plugins.insert("empty@m".into(), json!([]));
        write(&json!({"version":2,"plugins":plugins}));
        settings(
            json!({"user@m":false,"managed@m":true,"here@m":false,"elsewhere@m":true,"local@m":true,"empty@m":true}),
        );
        // Source :818-862 tests key presence, not enabled===true.
        for row in oracle()["predicates"].as_array().unwrap() {
            let id = row["id"].as_str().unwrap();
            assert_eq!(is_plugin_installed(id), row["installed"]);
            assert_eq!(is_plugin_globally_installed(id), row["global"]);
        }
        crate::bootstrap::state::set_original_cwd(previous);
        crate::utils::settings::settings_cache::reset_settings_cache();
        clear_installed_plugins_cache();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_registry_concurrent_writers_preserve_official_synchronous_turns() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("concurrent");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_installed_plugins_cache();
        let workers: Vec<_> = (0..8)
            .map(|n| {
                std::thread::spawn(move || {
                    add_installed_plugin(&format!("p{n}@m"), metadata(), PluginScope::User, None)
                        .unwrap()
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        // Source addInstalledPlugin contains no await between fresh read/save.
        assert_eq!(
            load_installed_plugins_from_disk()["plugins"]
                .as_object()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            load_installed_plugins_v2().lock().unwrap()["plugins"]
                .as_object()
                .unwrap()
                .len(),
            8
        );
        clear_installed_plugins_cache();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_registry_live_loader_retains_official_session_snapshot() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = root("loader");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        clear_installed_plugins_cache();
        settings(json!({"p@m":true}));
        let plugin = root.join("plugin");
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::write(plugin.join(".claude-plugin/plugin.json"), r#"{"name":"p"}"#).unwrap();
        // pluginLoader.ts:2005-2038 requires catalog membership independently
        // of registry state; the old fixture accidentally exercised a bypass.
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(root.join(".claude-plugin/marketplace.json"), r#"{"name":"m","owner":{"name":"Team"},"plugins":[{"name":"p","source":{"source":"github","repo":"team/p"}}]}"#).unwrap();
        std::fs::write(root.join("known_marketplaces.json"), json!({"m":{
            "source":{"source":"directory","path":root},"installLocation":root,"lastUpdated":"2026-09-14T00:00:00.000Z"
        }}).to_string()).unwrap();
        let initial = get_in_memory_installed_plugins();
        let mut meta = metadata();
        meta.install_path = plugin.to_string_lossy().into_owned();
        add_installed_plugin("p@m", meta, PluginScope::User, None).unwrap();
        let before = super::super::plugin_loader::load_all_plugins_cache_only_from_sync();
        assert!(!before.enabled.iter().any(|p| p.source == "p@m"));
        assert!(Arc::ptr_eq(&initial, &get_in_memory_installed_plugins()));
        // CC pluginLoader.ts:1959 consumes the session snapshot, not fresh memo.
        reset_in_memory_state();
        // Both source memoized layers must be invalidated to observe a new session snapshot.
        super::super::plugin_loader::clear_plugin_cache(None);
        let after = super::super::plugin_loader::load_all_plugins_cache_only_from_sync();
        assert!(after.enabled.iter().any(|p| p.source == "p@m"));
        clear_installed_plugins_cache();
        crate::utils::settings::settings_cache::reset_settings_cache();
        std::fs::remove_dir_all(root).unwrap();
    }
}
