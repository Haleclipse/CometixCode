//! Maps to: CC utils/plugins/pluginFlagging.ts.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};

const FLAGGED_PLUGINS_FILENAME: &str = "flagged-plugins.json";
const SEEN_EXPIRY_MS: i64 = 48 * 60 * 60 * 1000;
/// Maps to: CC pluginFlagging.ts#FlaggedPlugin.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedPlugin {
    pub flagged_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
}
/// Maps to: CC pluginFlagging.ts module cache Record carrier.
pub type FlaggedPlugins = IndexMap<String, FlaggedPlugin>;
static CACHE: LazyLock<Mutex<Option<FlaggedPlugins>>> = LazyLock::new(|| Mutex::new(None));
/// Maps to: CC pluginFlagging.ts#getFlaggedPluginsPath.
fn get_flagged_plugins_path() -> std::path::PathBuf {
    super::plugin_directories::get_plugins_directory().join(FLAGGED_PLUGINS_FILENAME)
}
/// Maps to: CC pluginFlagging.ts#parsePluginsData.
fn parse_plugins_data(content: &str) -> anyhow::Result<FlaggedPlugins> {
    let parsed: serde_json::Value = crate::utils::slow_operations::json_parse(content)?.to_json();
    let entries: Vec<(String, &serde_json::Value)> = match parsed.get("plugins") {
        Some(serde_json::Value::Object(map)) => {
            crate::utils::process_env::ecmascript_object_entries(map)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect()
        }
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => return Ok(IndexMap::new()),
    };
    let mut result = IndexMap::new();
    for (id, entry) in entries {
        if let Some(flagged_at) = entry.get("flaggedAt").and_then(|v| v.as_str()) {
            result.insert(
                id,
                FlaggedPlugin {
                    flagged_at: flagged_at.into(),
                    seen_at: entry
                        .get("seenAt")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                },
            );
        }
    }
    Ok(result)
}
/// Maps to: CC pluginFlagging.ts#readFromDisk.
async fn read_from_disk() -> FlaggedPlugins {
    match tokio::fs::read(get_flagged_plugins_path()).await {
        Ok(bytes) => parse_plugins_data(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        Err(_) => IndexMap::new(),
    }
}
/// Maps to: CC pluginFlagging.ts#writeToDisk.
async fn write_to_disk(plugins: FlaggedPlugins) {
    let path = get_flagged_plugins_path();
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).expect("OS entropy for flagged plugin temporary file");
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temp = std::path::PathBuf::from(format!("{}.{suffix}.tmp", path.display()));
    let result: anyhow::Result<()> = async {
        crate::utils::fs_operations::mkdir(
            &super::plugin_directories::get_plugins_directory(),
            None,
        )
        .await?;
        let content = crate::utils::slow_operations::json_stringify(
            &serde_json::json!({"plugins":plugins}),
            2,
        );
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        use tokio::io::AsyncWriteExt;
        let mut file = options.open(&temp).await?;
        file.write_all(content.as_bytes()).await?;
        drop(file);
        tokio::fs::rename(&temp, &path).await?;
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(plugins);
        Ok(())
    }
    .await;
    if let Err(error) = result {
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        let _ = tokio::fs::remove_file(temp).await;
    }
}
/// Maps to: CC pluginFlagging.ts#loadFlaggedPlugins.
pub async fn load_flagged_plugins() {
    let mut all = read_from_disk().await;
    let now = chrono::Utc::now().timestamp_millis();
    let old_len = all.len();
    all.retain(|_, entry| {
        !entry
            .seen_at
            .as_ref()
            .filter(|s| !s.is_empty())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|seen| now - seen.timestamp_millis() >= SEEN_EXPIRY_MS)
    });
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(all.clone());
    if old_len != all.len() {
        write_to_disk(all).await;
    }
}
/// Maps to: CC pluginFlagging.ts#getFlaggedPlugins.
pub fn get_flagged_plugins() -> FlaggedPlugins {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}
/// Maps to: CC pluginFlagging.ts#addFlaggedPlugin.
pub async fn add_flagged_plugin(plugin_id: &str) {
    if CACHE.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        let value = read_from_disk().await;
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
    let mut updated = get_flagged_plugins();
    updated.insert(
        plugin_id.into(),
        FlaggedPlugin {
            flagged_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            seen_at: None,
        },
    );
    write_to_disk(updated).await;
    crate::utils::debug::log_for_debugging(&format!("Flagged plugin: {plugin_id}"));
}
/// Maps to: CC pluginFlagging.ts#markFlaggedPluginsSeen.
pub async fn mark_flagged_plugins_seen(plugin_ids: &[String]) {
    if CACHE.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        let value = read_from_disk().await;
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut updated = get_flagged_plugins();
    let mut changed = false;
    for id in plugin_ids {
        if let Some(entry) = updated.get_mut(id)
            && entry.seen_at.as_ref().is_none_or(|s| s.is_empty())
        {
            entry.seen_at = Some(now.clone());
            changed = true;
        }
    }
    if changed {
        write_to_disk(updated).await;
    }
}
/// Maps to: CC pluginFlagging.ts#removeFlaggedPlugin.
pub async fn remove_flagged_plugin(plugin_id: &str) {
    if CACHE.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        let value = read_from_disk().await;
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
    let mut rest = get_flagged_plugins();
    if rest.shift_remove(plugin_id).is_none() {
        return;
    }
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(rest.clone());
    write_to_disk(rest).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flagged_data_matches_official_field_filtering() {
        // Original unchanged function oracle: plugin-panel-0914/pure-oracle.json.
        let value = parse_plugins_data(
            r#"{"plugins":{"a":{"flaggedAt":"x","seenAt":"","extra":true},"b":{"flaggedAt":3}}}"#,
        )
        .unwrap();
        assert_eq!(value.len(), 1);
        assert_eq!(value["a"].seen_at.as_deref(), Some(""));
        assert!(
            parse_plugins_data(r#"{"plugins":null}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_plugins_data("broken").is_err());
    }
}
