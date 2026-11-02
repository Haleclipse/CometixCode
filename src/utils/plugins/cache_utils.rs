//! Maps to: CC `utils/plugins/cacheUtils.ts`.
use crate::utils::debug::log_for_debugging;
use std::path::{Path, PathBuf};

const ORPHANED_AT_FILENAME: &str = ".orphaned_at";
const CLEANUP_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Maps to: CC cacheUtils.ts:24-44#clearAllPluginCaches.
pub fn clear_all_plugin_caches() {
    super::plugin_loader::clear_plugin_cache(None);
    super::load_plugin_commands::clear_plugin_command_cache();
    super::load_plugin_commands::clear_plugin_skills_cache();
    // Agent/output-style caches retain their existing separate owner boundary.
    super::load_plugin_hooks::clear_plugin_hook_cache();
    if crate::bootstrap::state::get_registered_hooks().is_some() {
        crate::utils::process_runtime::runtime_handle_for_detached_work()
            .expect("plugin hook pruning requires the process runtime")
            .spawn(async {
                if let Err(error) = super::load_plugin_hooks::prune_removed_plugin_hooks().await {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                }
            });
    }
    super::plugin_options_storage::clear_plugin_options_cache();
}
/// Maps to: CC cacheUtils.ts:46-52#clearAllCaches.
pub fn clear_all_caches() {
    clear_all_plugin_caches();
    crate::commands::clear_commands_cache();
    crate::utils::attachments::reset_sent_skill_names();
}
/// Maps to: CC cacheUtils.ts:58-66#markPluginVersionOrphaned.
pub async fn mark_plugin_version_orphaned(version_path: &Path) {
    if let Err(error) = tokio::fs::write(
        get_orphaned_at_path(version_path),
        chrono::Utc::now().timestamp_millis().to_string(),
    )
    .await
    {
        log_for_debugging(&format!(
            "Failed to write .orphaned_at: {}: {error}",
            version_path.display()
        ));
    }
}
/// Maps to: CC cacheUtils.ts:126-128#getOrphanedAtPath.
fn get_orphaned_at_path(version_path: &Path) -> PathBuf {
    version_path.join(ORPHANED_AT_FILENAME)
}
/// Maps to: CC cacheUtils.ts:130-140#removeOrphanedAtMarker.
async fn remove_orphaned_at_marker(version_path: &Path) {
    if let Err(error) = tokio::fs::remove_file(get_orphaned_at_path(version_path)).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log_for_debugging(&format!(
            "Failed to remove .orphaned_at: {}: {error}",
            version_path.display()
        ));
    }
}
/// Maps to: CC cacheUtils.ts:142-157#getInstalledVersionPaths.
fn get_installed_version_paths() -> Vec<PathBuf> {
    let data = super::installed_plugins_manager::load_installed_plugins_from_disk();
    let mut paths = Vec::new();
    for entries in data["plugins"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
    {
        for entry in entries.as_array().into_iter().flatten() {
            if let Some(path) = entry["installPath"].as_str() {
                let path = PathBuf::from(path);
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    paths
}
/// Maps to: CC cacheUtils.ts:159-190#processOrphanedPluginVersion.
async fn process_orphaned_plugin_version(version_path: &Path, now: i64) {
    let orphaned_at = match tokio::fs::metadata(get_orphaned_at_path(version_path)).await {
        Ok(stat) => match stat.modified() {
            Ok(time) => time
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            Err(error) => {
                log_for_debugging(&format!(
                    "Failed to stat orphaned marker: {}: {error}",
                    version_path.display()
                ));
                return;
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            mark_plugin_version_orphaned(version_path).await;
            return;
        }
        Err(error) => {
            log_for_debugging(&format!(
                "Failed to stat orphaned marker: {}: {error}",
                version_path.display()
            ));
            return;
        }
    };
    if now - orphaned_at > CLEANUP_AGE_MS
        && let Err(error) = tokio::fs::remove_dir_all(version_path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log_for_debugging(&format!(
            "Failed to delete orphaned version: {}: {error}",
            version_path.display()
        ));
    }
}
/// Maps to: CC cacheUtils.ts:192-202#removeIfEmpty.
async fn remove_if_empty(path: &Path) {
    if read_subdirs(path).await.is_empty()
        && let Err(error) = tokio::fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log_for_debugging(&format!(
            "Failed to remove empty dir: {}: {error}",
            path.display()
        ));
    }
}
/// Maps to: CC cacheUtils.ts:204-211#readSubdirs.
async fn read_subdirs(path: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(path).await else {
        return vec![];
    };
    let mut result = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                    result.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
            Ok(None) => break,
            Err(_) => return vec![],
        }
    }
    result.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    result
}
/// Maps to: CC cacheUtils.ts:77-124#cleanupOrphanedPluginVersionsInBackground.
pub async fn cleanup_orphaned_plugin_versions_in_background() {
    if super::zip_cache::is_plugin_zip_cache_enabled() {
        return;
    }
    let installed = get_installed_version_paths();
    futures::future::join_all(installed.iter().map(|path| remove_orphaned_at_marker(path))).await;
    let cache = super::plugin_loader::get_plugin_cache_path();
    let now = chrono::Utc::now().timestamp_millis();
    for marketplace in read_subdirs(&cache).await {
        let marketplace = cache.join(marketplace);
        for plugin in read_subdirs(&marketplace).await {
            let plugin = marketplace.join(plugin);
            for version in read_subdirs(&plugin).await {
                let version = plugin.join(version);
                if !installed.contains(&version) {
                    process_orphaned_plugin_version(&version, now).await;
                }
            }
            remove_if_empty(&plugin).await;
        }
        remove_if_empty(&marketplace).await;
    }
}
