//! Persistence for proxy channel configurations and caches in `~/.claude/`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::utils::config::get_config_home;

use super::types::{ChannelConfigFile, ChannelModelsCache, ProxyActiveConfig};

/// Path to `~/.claude/setting_proxy_active.json`.
pub fn active_proxy_path() -> PathBuf {
    get_config_home().join("setting_proxy_active.json")
}

/// Path to `~/.claude/setting_<id>.json`.
pub fn channel_config_path(id: &str) -> PathBuf {
    get_config_home().join(format!("setting_{id}.json"))
}

/// Path to `~/.claude/setting_<id>_models.json`.
pub fn channel_models_cache_path(id: &str) -> PathBuf {
    get_config_home().join(format!("setting_{id}_models.json"))
}

/// Load the active proxy channel ID from `setting_proxy_active.json`.
pub fn get_active_channel_id() -> Option<String> {
    let path = active_proxy_path();
    let content = fs::read_to_string(path).ok()?;
    let config: ProxyActiveConfig = serde_json::from_str(&content).ok()?;
    config.active.filter(|s| !s.trim().is_empty())
}

/// Set or clear the active proxy channel ID in `setting_proxy_active.json`.
pub fn set_active_channel_id(id: Option<&str>) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let config = ProxyActiveConfig {
        active: id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    };
    let json = serde_json::to_string_pretty(&config)?;
    fs::write(active_proxy_path(), json)?;
    Ok(())
}

/// Load `setting_<id>.json`.
pub fn load_channel_config(id: &str) -> Option<ChannelConfigFile> {
    let path = channel_config_path(id);
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save `setting_<id>.json`.
pub fn save_channel_config(id: &str, config: &ChannelConfigFile) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(config)?;
    fs::write(channel_config_path(id), json)?;
    Ok(())
}

/// Update specific environment variables in `setting_<id>.json`.
pub fn update_channel_env(id: &str, key_values: &[(&str, &str)]) -> Result<ChannelConfigFile> {
    let mut config = load_channel_config(id).unwrap_or_default();
    for (k, v) in key_values {
        config.env.insert(k.to_string(), v.to_string());
    }
    save_channel_config(id, &config)?;
    Ok(config)
}

/// Load `setting_<id>_models.json`.
pub fn load_channel_models_cache(id: &str) -> Option<ChannelModelsCache> {
    let path = channel_models_cache_path(id);
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save `setting_<id>_models.json`.
pub fn save_channel_models_cache(id: &str, cache: &ChannelModelsCache) -> Result<()> {
    let dir = get_config_home();
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(cache)?;
    fs::write(channel_models_cache_path(id), json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_active_proxy_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        assert_eq!(get_active_channel_id(), None);

        set_active_channel_id(Some("cpa")).unwrap();
        assert_eq!(get_active_channel_id(), Some("cpa".to_string()));

        set_active_channel_id(None).unwrap();
        assert_eq!(get_active_channel_id(), None);
    }

    #[test]
    fn test_channel_config_and_env_update() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::utils::env_utils::EnvVarGuard::set("CLAUDE_CONFIG_DIR", temp.path());

        assert!(load_channel_config("cpa").is_none());

        update_channel_env(
            "cpa",
            &[
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:8317"),
                ("ANTHROPIC_API_KEY", "sk-test"),
            ],
        )
        .unwrap();

        let loaded = load_channel_config("cpa").expect("should load config");
        assert_eq!(
            loaded.env.get("ANTHROPIC_BASE_URL"),
            Some(&"http://127.0.0.1:8317".to_string())
        );
        assert_eq!(
            loaded.env.get("ANTHROPIC_API_KEY"),
            Some(&"sk-test".to_string())
        );
    }
}
