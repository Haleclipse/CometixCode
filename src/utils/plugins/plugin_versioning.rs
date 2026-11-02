//! Maps to: CC `utils/plugins/pluginVersioning.ts`.
use super::schemas::PluginManifest;
use crate::utils::debug::log_for_debugging;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Maps to: CC pluginVersioning.ts:40-128#calculatePluginVersion.
pub async fn calculate_plugin_version(
    plugin_id: &str,
    source: &Value,
    manifest: Option<&PluginManifest>,
    install_path: Option<&Path>,
    provided_version: Option<&str>,
    git_commit_sha: Option<&str>,
) -> String {
    if let Some(version) = manifest
        .and_then(|m| m.version.as_deref())
        .filter(|v| !v.is_empty())
    {
        log_for_debugging(&format!(
            "Using manifest version for {plugin_id}: {version}"
        ));
        return version.into();
    }
    if let Some(version) = provided_version.filter(|v| !v.is_empty()) {
        log_for_debugging(&format!(
            "Using provided version for {plugin_id}: {version}"
        ));
        return version.into();
    }
    if let Some(sha) = git_commit_sha.filter(|v| !v.is_empty()) {
        let short: String = sha.chars().take(12).collect();
        if source.get("source").and_then(Value::as_str) == Some("git-subdir") {
            let path = source
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace('\\', "/");
            let normalized = path
                .strip_prefix("./")
                .unwrap_or(&path)
                .trim_end_matches('/');
            let hash = Sha256::digest(normalized.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let version = format!("{short}-{}", &hash[..8]);
            log_for_debugging(&format!(
                "Using git-subdir SHA+path version for {plugin_id}: {version} (path={normalized})"
            ));
            return version;
        }
        log_for_debugging(&format!(
            "Using pre-resolved git SHA for {plugin_id}: {short}"
        ));
        return short;
    }
    if let Some(path) = install_path
        && let Some(sha) = get_git_commit_sha(path).await.filter(|s| !s.is_empty())
    {
        let short: String = sha.chars().take(12).collect();
        log_for_debugging(&format!("Using git SHA for {plugin_id}: {short}"));
        return short;
    }
    log_for_debugging(&format!(
        "No version found for {plugin_id}, using 'unknown'"
    ));
    "unknown".into()
}
/// Maps to: CC pluginVersioning.ts:137-139#getGitCommitSha.
pub async fn get_git_commit_sha(path: &Path) -> Option<String> {
    crate::utils::git::git_filesystem::get_head_for_dir(path).await
}
/// Maps to: CC pluginVersioning.ts:151-172#getVersionFromPath.
pub fn get_version_from_path(path: &str) -> Option<String> {
    let parts: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
    let cache = parts
        .iter()
        .enumerate()
        .find(|(i, s)| **s == "cache" && *i > 0 && parts[*i - 1] == "plugins")?
        .0;
    parts.get(cache + 3).map(|s| (*s).into())
}
/// Maps to: CC pluginVersioning.ts:180-182#isVersionedPath.
pub fn is_versioned_path(path: &str) -> bool {
    get_version_from_path(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn version_matches_official_manifest_priority_and_subdir_normalization() {
        let manifest = PluginManifest {
            version: Some("1.0".into()),
            ..Default::default()
        };
        assert_eq!(
            calculate_plugin_version(
                "p@m",
                &Value::Null,
                Some(&manifest),
                None,
                Some("2"),
                Some("abcdef123456zz")
            )
            .await,
            "1.0"
        );
        let a = calculate_plugin_version(
            "p@m",
            &serde_json::json!({"source":"git-subdir","path":"./a\\b///"}),
            None,
            None,
            None,
            Some("abcdef123456zz"),
        )
        .await;
        let b = calculate_plugin_version(
            "p@m",
            &serde_json::json!({"source":"git-subdir","path":"a/b"}),
            None,
            None,
            None,
            Some("abcdef123456zz"),
        )
        .await;
        assert_eq!(a, b);
        assert!(a.starts_with("abcdef123456-"));
        assert_eq!(
            get_version_from_path("/plugins/cache/m/p/v/child").as_deref(),
            Some("v")
        );
    }
}
