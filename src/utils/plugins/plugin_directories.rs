//! Plugin directory helpers.
//!
//! Maps to: CC `utils/plugins/pluginDirectories.ts`.
//!
//! Cometix uses the official path construction for cache/seed/data lookups, but
//! provides source-owned cache/seed/data paths and explicit data lifecycle helpers.

use std::path::PathBuf;

use crate::utils::process_env::JsTruthy;

const PLUGINS_DIR: &str = "plugins";
const COWORK_PLUGINS_DIR: &str = "cowork_plugins";

/// Maps to CC `pluginDirectories.ts#getPluginsDirectoryName`.
fn get_plugins_directory_name() -> &'static str {
    if crate::bootstrap::state::get_use_cowork_plugins()
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_USE_COWORK_PLUGINS").as_deref(),
        )
    {
        COWORK_PLUGINS_DIR
    } else {
        PLUGINS_DIR
    }
}

/// Maps to CC `pluginDirectories.ts#getPluginsDirectory`.
pub fn get_plugins_directory() -> PathBuf {
    if let Some(path) = crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_CACHE_DIR").truthy() {
        return expand_tilde_path(&path);
    }
    crate::utils::env_utils::get_claude_config_home_dir().join(get_plugins_directory_name())
}

/// Maps to CC `pluginDirectories.ts#getPluginSeedDirs`.
pub fn get_plugin_seed_dirs() -> Vec<PathBuf> {
    let Some(raw) = crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_SEED_DIR").truthy() else {
        return Vec::new();
    };
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    raw.split(delimiter)
        .filter(|path| !path.is_empty())
        .map(expand_tilde_path)
        .collect()
}

/// Maps to CC `pluginDirectories.ts#pluginDataDirPath` `sanitizePluginId` helper.
fn sanitize_plugin_id(plugin_id: &str) -> String {
    plugin_id
        .encode_utf16()
        .map(|unit| {
            if unit <= 0x7f
                && ((unit as u8).is_ascii_alphanumeric() || matches!(unit as u8, b'-' | b'_'))
            {
                char::from(unit as u8)
            } else {
                '-'
            }
        })
        .collect()
}

/// Maps to CC `pluginDirectories.ts#pluginDataDirPath`.
pub fn plugin_data_dir_path(plugin_id: &str) -> PathBuf {
    get_plugins_directory()
        .join("data")
        .join(sanitize_plugin_id(plugin_id))
}

/// Maps to CC `permissions/pathValidation.ts#expandTilde` usage in
/// `pluginDirectories.ts#getPluginsDirectory` and `getPluginSeedDirs`.
pub fn expand_tilde_path(path: &str) -> PathBuf {
    PathBuf::from(crate::utils::permissions::path_validation::expand_tilde(
        path,
    ))
}

/// Maps to: CC pluginDirectories.ts#getPluginDataDir.
pub fn get_plugin_data_dir(plugin_id: &str) -> std::io::Result<PathBuf> {
    let dir = plugin_data_dir_path(plugin_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
/// Maps to: CC pluginDirectories.ts#getPluginDataDirSize result.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginDataDirSize {
    pub bytes: u64,
    pub human: String,
}
/// Maps to: CC pluginDirectories.ts#getPluginDataDirSize. Recursive walk follows
/// stat for leaf symlinks, but never traverses symlinks as directories.
pub async fn get_plugin_data_dir_size(
    plugin_id: &str,
) -> std::io::Result<Option<PluginDataDirSize>> {
    async fn walk(path: PathBuf) -> std::io::Result<u64> {
        let mut bytes = 0;
        let mut entries = tokio::fs::read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                bytes += Box::pin(walk(entry.path())).await?;
            } else if let Ok(metadata) = tokio::fs::metadata(entry.path()).await {
                bytes += metadata.len();
            }
        }
        Ok(bytes)
    }
    let bytes = match walk(plugin_data_dir_path(plugin_id)).await {
        Ok(bytes) => bytes,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::NotADirectory
            ) || e.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    Ok((bytes > 0).then(|| PluginDataDirSize {
        bytes,
        human: crate::utils::format::format_file_size(bytes as f64),
    }))
}
/// Maps to: CC pluginDirectories.ts#deletePluginDataDir.
pub async fn delete_plugin_data_dir(plugin_id: &str) {
    let dir = plugin_data_dir_path(plugin_id);
    if let Err(error) = crate::utils::fs_operations::native::rm(
        &dir,
        crate::utils::fs_operations::RmOptions {
            recursive: true,
            force: true,
        },
    )
    .await
    {
        crate::utils::debug::log_for_debugging(&format!(
            "Failed to delete plugin data dir {}: {error}",
            dir.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};

    #[test]
    fn plugin_directories_respect_cache_override_cowork_and_seed_precedence() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _restore = [
            EnvVarGuard::preserve("CLAUDE_CODE_PLUGIN_CACHE_DIR"),
            EnvVarGuard::preserve("CLAUDE_CODE_USE_COWORK_PLUGINS"),
            EnvVarGuard::preserve("CLAUDE_CODE_PLUGIN_SEED_DIR"),
        ];
        // `expandTilde` takes `homedir()`: pin it to a literal.
        let home = "/home/someone";
        let _home = EnvVarGuard::set(HOME_VAR, home);
        crate::utils::process_env::remove("CLAUDE_CODE_USE_COWORK_PLUGINS");
        crate::utils::process_env::remove("CLAUDE_CODE_PLUGIN_SEED_DIR");
        crate::utils::process_env::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", "~/custom-plugins");
        assert_eq!(
            get_plugins_directory(),
            PathBuf::from(&home).join("custom-plugins")
        );

        crate::utils::process_env::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", "");
        assert_eq!(
            get_plugins_directory(),
            crate::utils::env_utils::get_claude_config_home_dir().join(PLUGINS_DIR)
        );

        crate::utils::process_env::remove("CLAUDE_CODE_PLUGIN_CACHE_DIR");
        crate::utils::process_env::set("CLAUDE_CODE_USE_COWORK_PLUGINS", "1");
        assert!(get_plugins_directory().ends_with(COWORK_PLUGINS_DIR));

        let delimiter = if cfg!(windows) { ';' } else { ':' };
        crate::utils::process_env::set(
            "CLAUDE_CODE_PLUGIN_SEED_DIR",
            format!("~/seed-one{delimiter}/tmp/seed-two"),
        );
        let seeds = get_plugin_seed_dirs();
        assert_eq!(seeds[0], PathBuf::from(&home).join("seed-one"));
        assert_eq!(seeds[1], PathBuf::from("/tmp/seed-two"));

        crate::utils::process_env::remove("CLAUDE_CODE_PLUGIN_SEED_DIR");
        crate::utils::process_env::remove("CLAUDE_CODE_USE_COWORK_PLUGINS");
    }

    #[test]
    fn plugin_data_dir_path_sanitizes_plugin_id_without_creating_directory() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _restore = EnvVarGuard::preserve("CLAUDE_CODE_PLUGIN_CACHE_DIR");
        let root = std::env::temp_dir().join(format!(
            "cometix-plugin-data-{}",
            uuid::Uuid::new_v4().simple()
        ));
        crate::utils::process_env::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        let path = plugin_data_dir_path("plugin/name@market.place");
        assert_eq!(path, root.join("data").join("plugin-name-market-place"));
        assert!(!path.exists(), "read-only helper must not mkdir data dirs");
        crate::utils::process_env::remove("CLAUDE_CODE_PLUGIN_CACHE_DIR");
        let _ = std::fs::remove_dir_all(root);
    }
}
