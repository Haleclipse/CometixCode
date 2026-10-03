//! MCPB/user-config helpers for plugin MCP servers.
//!
//! Maps to: CC `utils/plugins/mcpbHandler.ts`.
//!
//! User-config loading, validation and sensitive split persistence.
//! MCPB archive materialization is a separate source-defined chain.

use serde_json::Value;
// Native std::path boundary for the source's Node path.join calls. Concatenate
// first (a later absolute fragment does not replace the base), then normalize
// lexically. This is neither filesystem canonicalization nor application policy.
macro_rules! mcpb_path {
    ($($part:expr),+ $(,)?) => {{
        let mut joined = String::new();
        $(let part=$part;let part=AsRef::<std::path::Path>::as_ref(&part).to_string_lossy();if !part.is_empty(){if !joined.is_empty(){joined.push(std::path::MAIN_SEPARATOR);}joined.push_str(&part);})+
        let mut normalized=std::path::PathBuf::new();
        for component in std::path::Path::new(&joined).components(){match component{
            std::path::Component::CurDir=>{},
            std::path::Component::ParentDir=>{if normalized.file_name().is_some_and(|s|s!=".."){normalized.pop();}else if !normalized.has_root(){normalized.push("..");}},
            c=>normalized.push(c.as_os_str()),
        }}
        if normalized.as_os_str().is_empty(){normalized.push(".");}normalized
    }};
}

/// Maps to: CC `utils/plugins/mcpbHandler.ts:124-126` `serverSecretsKey`.
pub fn server_secrets_key(plugin_id: &str, server_name: &str) -> String {
    format!("{plugin_id}/{server_name}")
}

/// Maps to: CC `utils/plugins/mcpbHandler.ts:141-164` `loadMcpServerUserConfig`.
pub fn load_mcp_server_user_config(
    plugin_id: &str,
    server_name: &str,
) -> Option<serde_json::Map<String, Value>> {
    let settings = crate::utils::settings::get_initial_settings();
    let mut values = settings
        .plugin_configs
        .as_ref()
        .and_then(|configs| configs.get(plugin_id))
        .and_then(|plugin_config| plugin_config.get("mcpServers"))
        .and_then(|servers| servers.get(server_name))
        .and_then(Value::as_object)
        .cloned();

    let credentials = crate::utils::secure_storage::get_secure_storage().read();
    let secrets_key = server_secrets_key(plugin_id, server_name);
    let sensitive = credentials
        .as_ref()
        .and_then(|storage| storage.get("pluginSecrets"))
        .and_then(|plugin_secrets| plugin_secrets.get(&secrets_key))
        .and_then(Value::as_object);

    if values.is_none() && sensitive.is_none() {
        return None;
    }

    let values = values.get_or_insert_with(serde_json::Map::new);
    if let Some(sensitive) = sensitive {
        for (key, value) in sensitive {
            values.insert(key.clone(), value.clone());
        }
    }
    Some(values.clone())
}

/// Maps to: CC mcpbHandler.ts#UserConfigSchema/UserConfigValues consumer carriers.
pub type UserConfigSchema = Value;
pub type UserConfigValues = serde_json::Map<String, Value>;
/// Maps to: CC mcpbHandler.ts#validateUserConfig return value.
#[derive(Clone, Debug, PartialEq)]
pub struct UserConfigValidation {
    pub valid: bool,
    pub errors: Vec<String>,
}
/// Maps to: CC mcpbHandler.ts:346-407#validateUserConfig.
pub fn validate_user_config(
    values: &UserConfigValues,
    schema: &UserConfigSchema,
) -> UserConfigValidation {
    let mut errors = Vec::new();
    if let Some(fields) = schema.as_object() {
        for (key, field) in crate::utils::process_env::ecmascript_object_entries(fields) {
            let value = values.get(key);
            let title = field
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(key);
            let absent = value.is_none() || value == Some(&Value::String(String::new()));
            if field.get("required").and_then(Value::as_bool) == Some(true) && absent {
                errors.push(format!("{title} is required but not provided"));
                continue;
            }
            if absent {
                continue;
            }
            let value = value.unwrap();
            match field.get("type").and_then(Value::as_str) {
                Some("string") => {
                    if let Some(array) = value.as_array() {
                        if field.get("multiple").and_then(Value::as_bool) != Some(true) {
                            errors.push(format!("{title} must be a string, not an array"));
                        } else if !array.iter().all(Value::is_string) {
                            errors.push(format!("{title} must be an array of strings"));
                        }
                    } else if !value.is_string() {
                        errors.push(format!("{title} must be a string"));
                    }
                }
                Some("number") if !value.is_number() => {
                    errors.push(format!("{title} must be a number"))
                }
                Some("boolean") if !value.is_boolean() => {
                    errors.push(format!("{title} must be a boolean"))
                }
                Some("file" | "directory") if !value.is_string() => {
                    errors.push(format!("{title} must be a path string"))
                }
                _ => {}
            }
            if field.get("type").and_then(Value::as_str) == Some("number") {
                if let Some(number) = value.as_f64() {
                    if field
                        .get("min")
                        .and_then(Value::as_f64)
                        .is_some_and(|n| number < n)
                    {
                        errors.push(format!("{title} must be at least {}", field["min"]));
                    }
                    if field
                        .get("max")
                        .and_then(Value::as_f64)
                        .is_some_and(|n| number > n)
                    {
                        errors.push(format!("{title} must be at most {}", field["max"]));
                    }
                }
            }
        }
    }
    UserConfigValidation {
        valid: errors.is_empty(),
        errors,
    }
}
/// Maps to: CC mcpbHandler.ts:193-341#saveMcpServerUserConfig.
pub fn save_mcp_server_user_config(
    plugin_id: &str,
    server_name: &str,
    config: &UserConfigValues,
    schema: &UserConfigSchema,
) -> anyhow::Result<()> {
    let result: anyhow::Result<()> = (|| {
        let mut non_sensitive = UserConfigValues::new();
        let mut sensitive = UserConfigValues::new();
        for (key, value) in config {
            if schema
                .get(key)
                .and_then(|s| s.get("sensitive"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                // Source String(value) for the supported user-config scalar/array domain.
                let text = match value {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                    Value::Object(_) => "[object Object]".into(),
                    other => other.to_string(),
                };
                sensitive.insert(key.clone(), Value::String(text));
            } else {
                non_sensitive.insert(key.clone(), value.clone());
            }
        }
        let storage = crate::utils::secure_storage::get_secure_storage();
        let k = server_secrets_key(plugin_id, server_name);
        let first = storage.read();
        let existing_secure = first
            .as_ref()
            .and_then(|s| s.get("pluginSecrets"))
            .and_then(|s| s.get(&k))
            .and_then(Value::as_object);
        let mut scrubbed = existing_secure.cloned().unwrap_or_default();
        scrubbed.retain(|key, _| !non_sensitive.contains_key(key));
        let removed = existing_secure.map_or(0, |s| s.len()) - scrubbed.len();
        if !sensitive.is_empty() || removed > 0 {
            let mut existing = storage.read().unwrap_or_else(|| serde_json::json!({}));
            if !existing.get("pluginSecrets").is_some_and(Value::is_object) {
                existing["pluginSecrets"] = serde_json::json!({});
            }
            scrubbed.extend(sensitive.clone());
            existing["pluginSecrets"][&k] = Value::Object(scrubbed);
            let result = storage.update(&existing)?;
            if !result.success {
                anyhow::bail!("Failed to save sensitive config to secure storage for {k}");
            }
            if let Some(warning) = result.warning {
                crate::utils::debug::log_for_debugging(&format!(
                    "Server secrets save warning: {warning}"
                ));
            }
            if removed > 0 {
                crate::utils::debug::log_for_debugging(&format!(
                    "saveMcpServerUserConfig: scrubbed {removed} stale non-sensitive key(s) from secureStorage for {k}"
                ));
            }
        }
        let settings = crate::utils::settings::get_initial_settings();
        let old = settings
            .plugin_configs
            .as_ref()
            .and_then(|c| c.get(plugin_id))
            .and_then(|c| c.get("mcpServers"))
            .and_then(|c| c.get(server_name))
            .and_then(Value::as_object);
        let keys: Vec<_> = old
            .into_iter()
            .flat_map(|o| o.keys())
            .filter(|k| sensitive.contains_key(*k))
            .cloned()
            .collect();
        let removed = keys.len();
        let plain_count = non_sensitive.len();
        if !non_sensitive.is_empty() || removed > 0 {
            for key in keys {
                non_sensitive.insert(key, Value::Null);
            }
            let mut patch = serde_json::to_value(settings)?;
            if !patch.get("pluginConfigs").is_some_and(Value::is_object) {
                patch["pluginConfigs"] = serde_json::json!({});
            }
            if !patch["pluginConfigs"]
                .get(plugin_id)
                .is_some_and(Value::is_object)
            {
                patch["pluginConfigs"][plugin_id] = serde_json::json!({});
            }
            if !patch["pluginConfigs"][plugin_id]
                .get("mcpServers")
                .is_some_and(Value::is_object)
            {
                patch["pluginConfigs"][plugin_id]["mcpServers"] = serde_json::json!({});
            }
            patch["pluginConfigs"][plugin_id]["mcpServers"][server_name] =
                Value::Object(non_sensitive);
            crate::utils::settings::update_settings_for_source(
                crate::utils::settings::SettingSource::User,
                patch.as_object().unwrap(),
            )?;
            if removed > 0 {
                crate::utils::debug::log_for_debugging(&format!(
                    "saveMcpServerUserConfig: scrubbed {removed} plaintext sensitive key(s) from settings.json for {k}"
                ));
            }
        }
        crate::utils::debug::log_for_debugging(&format!(
            "Saved user config for {k} ({plain_count} non-sensitive, {} sensitive)",
            sensitive.len()
        ));
        Ok(())
    })();
    result.map_err(|e| {
        crate::utils::log::log_error(crate::utils::log::LogError::new(e.to_string()));
        anyhow::anyhow!("Failed to save user configuration for {plugin_id}/{server_name}: {e}")
    })
}

/// Maps to: CC mcpbHandler.ts#McpbLoadResult.
#[derive(Clone, Debug)]
pub struct McpbLoadResult {
    pub manifest: Value,
    pub mcp_config: Value,
    pub extracted_path: std::path::PathBuf,
    pub content_hash: String,
}
/// Maps to: CC mcpbHandler.ts#McpbNeedsConfigResult.
#[derive(Clone, Debug)]
pub struct McpbNeedsConfigResult {
    pub manifest: Value,
    pub extracted_path: std::path::PathBuf,
    pub content_hash: String,
    pub config_schema: Value,
    pub existing_config: UserConfigValues,
    pub validation_errors: Vec<String>,
}
/// Native representation of loadMcpbFile's discriminated return union.
#[derive(Clone, Debug)]
pub enum McpbFileResult {
    Loaded(McpbLoadResult),
    NeedsConfig(McpbNeedsConfigResult),
}
/// Maps to: CC mcpbHandler.ts#McpbCacheMetadata.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpbCacheMetadata {
    // Source never reads these two fields. They remain writer-owned metadata;
    // jsonParse's unchecked cast must not reject absent or arbitrary values.
    #[serde(skip_deserializing)]
    source: String,
    content_hash: String,
    extracted_path: std::path::PathBuf,
    cached_at: String,
    #[serde(skip_deserializing)]
    last_checked: String,
}
/// Maps to: CC mcpbHandler.ts#ProgressCallback. Native callbacks may report producer failure.
pub type ProgressCallback = dyn Fn(&str) -> anyhow::Result<()> + Send + Sync;
/// Maps to: CC mcpbHandler.ts#isMcpbSource.
pub fn is_mcpb_source(source: &str) -> bool {
    source.ends_with(".mcpb") || source.ends_with(".dxt")
}
/// Maps to: CC mcpbHandler.ts#isUrl.
fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}
/// Maps to: CC mcpbHandler.ts#generateContentHash.
fn generate_content_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_owned()
}
/// Maps to: CC mcpbHandler.ts#getMcpbCacheDir.
fn get_mcpb_cache_dir(plugin_path: &std::path::Path) -> std::path::PathBuf {
    mcpb_path!(plugin_path, ".mcpb-cache")
}
/// Maps to: CC mcpbHandler.ts#getMetadataPath.
fn get_metadata_path(cache_dir: &std::path::Path, source: &str) -> std::path::PathBuf {
    mcpb_path!(
        cache_dir,
        format!(
            "{}.metadata.json",
            &format!("{:x}", md5::compute(source))[..8]
        )
    )
}
/// Maps to: CC mcpbHandler.ts#loadCacheMetadata.
async fn load_cache_metadata(
    cache_dir: &std::path::Path,
    source: &str,
) -> Option<McpbCacheMetadata> {
    let result: anyhow::Result<McpbCacheMetadata> = async {
        let bytes = tokio::fs::read(get_metadata_path(cache_dir, source)).await?;
        Ok(serde_json::from_value(
            crate::utils::slow_operations::json_parse(&String::from_utf8_lossy(&bytes))?.to_json(),
        )?)
    }
    .await;
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            if !e
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                crate::utils::log::log_error(crate::utils::log::LogError::new(e.to_string()));
                crate::utils::debug::log_for_debugging_with_level(
                    &format!("Failed to load MCPB cache metadata: {e}"),
                    crate::utils::debug::DebugLogLevel::Error,
                );
            }
            None
        }
    }
}
/// Maps to: CC mcpbHandler.ts#saveCacheMetadata.
async fn save_cache_metadata(
    cache_dir: &std::path::Path,
    source: &str,
    metadata: McpbCacheMetadata,
) -> anyhow::Result<()> {
    crate::utils::fs_operations::mkdir(cache_dir, None).await?;
    tokio::fs::write(
        get_metadata_path(cache_dir, source),
        crate::utils::slow_operations::json_stringify(&serde_json::to_value(metadata)?, 2),
    )
    .await?;
    Ok(())
}
/// Maps to: CC mcpbHandler.ts#downloadMcpb.
async fn download_mcpb(
    url: &str,
    dest_path: &std::path::Path,
    on_progress: Option<&ProgressCallback>,
) -> anyhow::Result<Vec<u8>> {
    use super::fetch_telemetry::{
        PluginFetchOutcome, PluginFetchSource, classify_fetch_error, log_plugin_fetch,
    };
    crate::utils::debug::log_for_debugging(&format!("Downloading MCPB from {url}"));
    if let Some(cb) = on_progress {
        cb(&format!("Downloading {url}..."))?;
    }
    let start = std::time::Instant::now();
    let mut fired = false;
    let result: anyhow::Result<Vec<u8>> = async {
        // CC `axios.get` (`mcpbHandler.ts:495-507`), through the global
        // interceptor.
        let client = crate::utils::proxy::create_axios_instance()?
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        let mut response = client.get(url).send().await?.error_for_status()?;
        let total = response.content_length();
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            data.extend_from_slice(&chunk);
            if let (Some(total), Some(cb)) = (total.filter(|v| *v != 0), on_progress) {
                cb(&format!(
                    "Downloading... {}%",
                    (data.len() as f64 / total as f64 * 100.0).round()
                ))?;
            }
        }
        log_plugin_fetch(
            PluginFetchSource::Mcpb,
            Some(url),
            PluginFetchOutcome::Success,
            start.elapsed().as_secs_f64() * 1000.0,
            None,
        );
        fired = true;
        tokio::fs::write(dest_path, &data).await?;
        crate::utils::debug::log_for_debugging(&format!(
            "Downloaded {} bytes to {}",
            data.len(),
            dest_path.display()
        ));
        if let Some(cb) = on_progress {
            cb("Download complete")?;
        }
        Ok(data)
    }
    .await;
    match result {
        Ok(data) => Ok(data),
        Err(error) => {
            if !fired {
                let kind = classify_fetch_error(&error.to_string());
                log_plugin_fetch(
                    PluginFetchSource::Mcpb,
                    Some(url),
                    PluginFetchOutcome::Failure,
                    start.elapsed().as_secs_f64() * 1000.0,
                    Some(kind),
                );
            }
            let error = anyhow::anyhow!("Failed to download MCPB file from {url}: {error}");
            crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
            Err(error)
        }
    }
}
/// Maps to: CC mcpbHandler.ts#extractMcpbContents.
async fn extract_mcpb_contents(
    unzipped: &indexmap::IndexMap<String, Vec<u8>>,
    extract_path: &std::path::Path,
    modes: &std::collections::HashMap<String, u32>,
    on_progress: Option<&ProgressCallback>,
) -> anyhow::Result<()> {
    if let Some(cb) = on_progress {
        cb("Extracting files...")?;
    }
    crate::utils::fs_operations::mkdir(extract_path, None).await?;
    let entries = crate::utils::process_env::ecmascript_object_entries(unzipped.iter())
        .into_iter()
        .filter(|(name, _)| !name.ends_with('/'))
        .collect::<Vec<_>>();
    let total = entries.len();
    for (index, (path, data)) in entries.into_iter().enumerate() {
        let full = mcpb_path!(extract_path, path);
        if let Some(dir) = full.parent() {
            if dir != extract_path {
                crate::utils::fs_operations::mkdir(dir, None).await?;
            }
        }
        if [".json", ".js", ".ts", ".txt", ".md", ".yml", ".yaml"]
            .iter()
            .any(|ext| path.ends_with(ext))
        {
            let text = String::from_utf8_lossy(data);
            tokio::fs::write(&full, text.strip_prefix('\u{feff}').unwrap_or(&text)).await?;
        } else {
            tokio::fs::write(&full, data).await?;
        }
        #[cfg(unix)]
        if let Some(mode) = modes.get(path).filter(|m| **m & 0o111 != 0) {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                tokio::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode & 0o777))
                    .await;
        }
        if (index + 1) % 10 == 0 {
            if let Some(cb) = on_progress {
                cb(&format!("Extracted {}/{total} files", index + 1))?;
            }
        }
    }
    crate::utils::debug::log_for_debugging(&format!(
        "Extracted {total} files to {}",
        extract_path.display()
    ));
    if let Some(cb) = on_progress {
        cb(&format!("Extraction complete ({total} files)"))?;
    }
    Ok(())
}
/// Maps to: CC mcpbHandler.ts#checkMcpbChanged.
pub async fn check_mcpb_changed(source: &str, plugin_path: &std::path::Path) -> bool {
    use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
    use crate::utils::errors::format_native_file_error;
    let Some(metadata) = load_cache_metadata(&get_mcpb_cache_dir(plugin_path), source).await else {
        return true;
    };
    if let Err(error) = tokio::fs::metadata(&metadata.extracted_path).await {
        if error.kind() == std::io::ErrorKind::NotFound {
            log_for_debugging(&format!(
                "MCPB extraction path missing: {}",
                metadata.extracted_path.display()
            ));
        } else {
            log_for_debugging_with_level(
                &format!(
                    "MCPB extraction path inaccessible: {}: {}",
                    metadata.extracted_path.display(),
                    format_native_file_error(&error, "stat", Some(&metadata.extracted_path))
                ),
                DebugLogLevel::Error,
            );
        }
        return true;
    }
    if !is_url(source) {
        let local_path = mcpb_path!(plugin_path, source);
        let stats = match tokio::fs::metadata(&local_path).await {
            Ok(stats) => stats,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    log_for_debugging(&format!(
                        "MCPB source file missing: {}",
                        local_path.display()
                    ));
                } else {
                    log_for_debugging_with_level(
                        &format!(
                            "MCPB source file inaccessible: {}: {}",
                            local_path.display(),
                            format_native_file_error(&error, "stat", Some(&local_path))
                        ),
                        DebugLogLevel::Error,
                    );
                }
                return true;
            }
        };
        let cached = chrono::DateTime::parse_from_rfc3339(&metadata.cached_at)
            .ok()
            .map(|v| v.timestamp_millis());
        // Node Math.floor(mtimeMs), including pre-epoch timestamps: Rust's
        // unsigned duration_since would silently discard negative file times.
        let file =
            stats
                .modified()
                .ok()
                .map(|time| match time.duration_since(std::time::UNIX_EPOCH) {
                    Ok(d) => d.as_millis() as i64,
                    Err(e) => {
                        -(e.duration().as_millis() as i64)
                            - i64::from(e.duration().subsec_nanos() % 1_000_000 != 0)
                    }
                });
        if let (Some(file), Some(cached)) = (file, cached) {
            if file > cached {
                // Native Date diagnostic rendering: chrono supplies host timezone;
                // JS's localized parenthetical timezone name is not available here.
                let display = |millis| {
                    chrono::DateTime::from_timestamp_millis(millis)
                        .map(|d| {
                            d.with_timezone(&chrono::Local)
                                .format("%a %b %d %Y %H:%M:%S GMT%z")
                                .to_string()
                        })
                        .unwrap_or_else(|| "Invalid Date".into())
                };
                log_for_debugging(&format!(
                    "MCPB file modified: {} > {}",
                    display(file),
                    display(cached)
                ));
                return true;
            }
        }
    }
    false
}
/// Maps to: CC mcpbHandler.ts#loadMcpbFile.
pub async fn load_mcpb_file(
    source: &str,
    plugin_path: &std::path::Path,
    plugin_id: &str,
    on_progress: Option<&ProgressCallback>,
    provided_user_config: Option<&UserConfigValues>,
    force_config_dialog: bool,
) -> anyhow::Result<McpbFileResult> {
    let cache_dir = get_mcpb_cache_dir(plugin_path);
    crate::utils::fs_operations::mkdir(&cache_dir, None).await?;
    crate::utils::debug::log_for_debugging(&format!("Loading MCPB from source: {source}"));
    let metadata = load_cache_metadata(&cache_dir, source).await;
    if let Some(metadata) = metadata {
        if !check_mcpb_changed(source, plugin_path).await {
            crate::utils::debug::log_for_debugging(&format!(
                "Using cached MCPB from {} (hash: {})",
                metadata.extracted_path.display(),
                metadata.content_hash
            ));
            let path = mcpb_path!(&metadata.extracted_path, "manifest.json");
            let bytes = tokio::fs::read(&path).await.map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    let error = anyhow::anyhow!("Cached manifest not found: {}", path.display());
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                    error
                } else {
                    error.into()
                }
            })?;
            let manifest =
                crate::utils::dxt::helpers::parse_and_validate_manifest_from_bytes(&bytes).await?;
            if let Some(schema) = manifest
                .get("user_config")
                .filter(|s| s.as_object().is_some_and(|v| !v.is_empty()))
            {
                let server = manifest["name"].as_str().unwrap();
                let saved = load_mcp_server_user_config(plugin_id, server);
                let empty = UserConfigValues::new();
                let config = provided_user_config.or(saved.as_ref()).unwrap_or(&empty);
                let validation = validate_user_config(config, schema);
                if force_config_dialog || !validation.valid {
                    return Ok(McpbFileResult::NeedsConfig(McpbNeedsConfigResult {
                        config_schema: schema.clone(),
                        manifest,
                        extracted_path: metadata.extracted_path,
                        content_hash: metadata.content_hash,
                        existing_config: saved.unwrap_or_default(),
                        validation_errors: validation.errors,
                    }));
                }
                if let Some(provided) = provided_user_config {
                    save_mcp_server_user_config(plugin_id, server, provided, schema)?;
                }
                let mcp_config =
                    generate_mcp_config(&manifest, &metadata.extracted_path, config).await?;
                return Ok(McpbFileResult::Loaded(McpbLoadResult {
                    manifest,
                    mcp_config,
                    extracted_path: metadata.extracted_path,
                    content_hash: metadata.content_hash,
                }));
            }
            let mcp_config = generate_mcp_config(
                &manifest,
                &metadata.extracted_path,
                &UserConfigValues::new(),
            )
            .await?;
            return Ok(McpbFileResult::Loaded(McpbLoadResult {
                manifest,
                mcp_config,
                extracted_path: metadata.extracted_path,
                content_hash: metadata.content_hash,
            }));
        }
    }
    let data = if is_url(source) {
        let dest = mcpb_path!(
            &cache_dir,
            format!("{}.mcpb", &format!("{:x}", md5::compute(source))[..8])
        );
        download_mcpb(source, &dest, on_progress).await?
    } else {
        if let Some(cb) = on_progress {
            cb(&format!("Loading {source}..."))?;
        }
        let local = mcpb_path!(plugin_path, source);
        tokio::fs::read(&local).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                let error = anyhow::anyhow!("MCPB file not found: {}", local.display());
                crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                error
            } else {
                error.into()
            }
        })?
    };
    let content_hash = generate_content_hash(&data);
    crate::utils::debug::log_for_debugging(&format!("MCPB content hash: {content_hash}"));
    if let Some(cb) = on_progress {
        cb("Extracting MCPB archive...")?;
    }
    let unzipped = crate::utils::dxt::zip::unzip_file(&data).await?;
    let modes = crate::utils::dxt::zip::parse_zip_modes(&data);
    let manifest_data = unzipped.get("manifest.json").ok_or_else(|| {
        let error = anyhow::anyhow!("No manifest.json found in MCPB file");
        crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
        error
    })?;
    let manifest =
        crate::utils::dxt::helpers::parse_and_validate_manifest_from_bytes(manifest_data).await?;
    crate::utils::debug::log_for_debugging(&format!(
        "MCPB manifest: {} v{} by {}",
        manifest["name"].as_str().unwrap_or("undefined"),
        manifest["version"].as_str().unwrap_or("undefined"),
        manifest["author"]["name"].as_str().unwrap_or("undefined")
    ));
    if manifest.get("server").is_none_or(Value::is_null) {
        crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
            "MCPB manifest for \"{}\" does not define a server configuration",
            manifest["name"].as_str().unwrap_or("undefined")
        )));
        anyhow::bail!(
            "MCPB manifest for \"{}\" does not define a server configuration",
            manifest["name"].as_str().unwrap_or_default()
        );
    }
    let extracted_path = mcpb_path!(&cache_dir, &content_hash);
    extract_mcpb_contents(&unzipped, &extracted_path, &modes, on_progress).await?;
    if let Some(schema) = manifest
        .get("user_config")
        .filter(|s| s.as_object().is_some_and(|v| !v.is_empty()))
    {
        let server = manifest["name"].as_str().unwrap();
        let saved = load_mcp_server_user_config(plugin_id, server);
        let empty = UserConfigValues::new();
        let config = provided_user_config.or(saved.as_ref()).unwrap_or(&empty);
        let validation = validate_user_config(config, schema);
        if !validation.valid {
            save_cache_metadata(
                &cache_dir,
                source,
                McpbCacheMetadata {
                    source: source.into(),
                    content_hash: content_hash.clone(),
                    extracted_path: extracted_path.clone(),
                    cached_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    last_checked: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                },
            )
            .await?;
            return Ok(McpbFileResult::NeedsConfig(McpbNeedsConfigResult {
                config_schema: schema.clone(),
                manifest,
                extracted_path,
                content_hash,
                existing_config: saved.unwrap_or_default(),
                validation_errors: validation.errors,
            }));
        }
        if let Some(provided) = provided_user_config {
            save_mcp_server_user_config(plugin_id, server, provided, schema)?;
        }
        if let Some(cb) = on_progress {
            cb("Generating MCP server configuration...")?;
        }
        let mcp_config = generate_mcp_config(&manifest, &extracted_path, config).await?;
        save_cache_metadata(
            &cache_dir,
            source,
            McpbCacheMetadata {
                source: source.into(),
                content_hash: content_hash.clone(),
                extracted_path: extracted_path.clone(),
                cached_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                last_checked: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            },
        )
        .await?;
        return Ok(McpbFileResult::Loaded(McpbLoadResult {
            manifest,
            mcp_config,
            extracted_path,
            content_hash,
        }));
    }
    if let Some(cb) = on_progress {
        cb("Generating MCP server configuration...")?;
    }
    let mcp_config =
        generate_mcp_config(&manifest, &extracted_path, &UserConfigValues::new()).await?;
    save_cache_metadata(
        &cache_dir,
        source,
        McpbCacheMetadata {
            source: source.into(),
            content_hash: content_hash.clone(),
            extracted_path: extracted_path.clone(),
            cached_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            last_checked: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
    )
    .await?;
    Ok(McpbFileResult::Loaded(McpbLoadResult {
        manifest,
        mcp_config,
        extracted_path,
        content_hash,
    }))
}
/// Maps to: CC mcpbHandler.ts#generateMcpConfig.
async fn generate_mcp_config(
    manifest: &Value,
    extracted_path: &std::path::Path,
    user_config: &UserConfigValues,
) -> anyhow::Result<Value> {
    // Dependency carrier for @anthropic-ai/mcpb 2.1.2 dist/shared/config.js.
    // Keep its required-value check BEFORE default merging.
    let invalid =
        |value: Option<&Value>| value.is_none_or(|v| v.is_null() || v.as_str() == Some(""));
    let missing = manifest["user_config"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(key, schema)| {
            schema["required"] == true
                && (invalid(user_config.get(key))
                    || user_config
                        .get(key)
                        .and_then(Value::as_array)
                        .is_some_and(|array| {
                            array.is_empty() || array.iter().any(|v| invalid(Some(v)))
                        }))
        });
    let Some(base) = manifest["server"]
        .get("mcp_config")
        .filter(|v| !v.is_null())
    else {
        anyhow::bail!(
            "Failed to generate MCP server configuration from manifest \"{}\"",
            manifest["name"].as_str().unwrap_or_default()
        );
    };
    if missing {
        anyhow::bail!(
            "Failed to generate MCP server configuration from manifest \"{}\"",
            manifest["name"].as_str().unwrap_or_default()
        );
    }
    let mut result = base.clone();
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    };
    if let Some(overrides) = base["platform_overrides"].get(platform) {
        for key in ["command", "args", "env"] {
            if let Some(value) = overrides
                .get(key)
                .filter(|v| !v.is_null() && v.as_str() != Some("") && **v != Value::Bool(false))
            {
                result[key] = value.clone();
            }
        }
    }
    let mut variables = indexmap::IndexMap::<String, Value>::from([
        (
            "__dirname".into(),
            Value::String(extracted_path.to_string_lossy().into_owned()),
        ),
        ("pathSeparator".into(), Value::String("/".into())),
        ("/".into(), Value::String("/".into())),
    ]);
    for (key, value) in crate::utils::system_directories::get_system_directories(None) {
        variables.insert(key, Value::String(value));
    }
    let mut merged = UserConfigValues::new();
    for (key, option) in manifest["user_config"].as_object().into_iter().flatten() {
        if let Some(default) = option.get("default") {
            merged.insert(key.clone(), default.clone());
        }
    }
    merged.extend(user_config.clone());
    let js_string = |v: &Value| {
        if v.is_null() {
            "null".to_owned()
        } else {
            crate::utils::json::JsoncValue::from_json(v.clone())
                .array_string()
                .unwrap_or_else(|_| "[object Object]".into())
        }
    };
    for (key, value) in merged {
        variables.insert(
            format!("user_config.{key}"),
            if let Value::Array(array) = value {
                Value::Array(array.iter().map(|v| Value::String(js_string(v))).collect())
            } else {
                Value::String(js_string(&value))
            },
        );
    }
    // Recursive dependency conversion, not a second application configuration algorithm.
    fn replace_variables(value: &Value, variables: &indexmap::IndexMap<String, Value>) -> Value {
        match value {
            Value::String(text) => {
                let mut result = text.clone();
                for (key, replacement) in variables {
                    let regex = regress::Regex::new(&format!(r"\$\{{{key}\}}"));
                    if let Ok(regex) = regex {
                        if replacement.is_array() {
                            continue;
                        }
                        let replacement = replacement.as_str().unwrap_or_default();
                        let mut output = String::new();
                        let mut offset = 0;
                        for found in regex.find_iter(&result) {
                            output.push_str(&result[offset..found.range().start]);
                            let matched = &result[found.range()];
                            let mut chars = replacement.chars().peekable();
                            while let Some(c) = chars.next() {
                                if c == '$' {
                                    match chars.peek().copied() {
                                        Some('$') => {
                                            chars.next();
                                            output.push('$');
                                        }
                                        Some('&') => {
                                            chars.next();
                                            output.push_str(matched);
                                        }
                                        Some('`') => {
                                            chars.next();
                                            output.push_str(&result[..found.range().start]);
                                        }
                                        Some('\'') => {
                                            chars.next();
                                            output.push_str(&result[found.range().end..]);
                                        }
                                        _ => output.push(c),
                                    }
                                } else {
                                    output.push(c);
                                }
                            }
                            offset = found.range().end;
                        }
                        output.push_str(&result[offset..]);
                        result = output;
                    }
                }
                Value::String(result)
            }
            Value::Array(array) => {
                let mut result = Vec::new();
                for item in array {
                    let variable_name = item
                        .as_str()
                        .and_then(|s| {
                            s.strip_prefix("${user_config.")
                                .and_then(|s| s.strip_suffix('}'))
                        })
                        .filter(|s| !s.is_empty() && !s.contains('}'));
                    let variable =
                        variable_name.and_then(|s| variables.get(&format!("user_config.{s}")));
                    if let Some(value) = variable.filter(|v| v.as_str() != Some("")) {
                        if let Value::Array(values) = value {
                            result.extend(values.clone());
                        } else {
                            result.push(value.clone());
                        }
                    } else if variable_name.is_some() {
                        result.push(item.clone());
                    } else {
                        result.push(replace_variables(item, variables));
                    }
                }
                Value::Array(result)
            }
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(k, v)| (k.clone(), replace_variables(v, variables)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
    Ok(replace_variables(&result, &variables))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::io::Write;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cometix-mcpb-handler-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn mcp_server_user_config_merges_settings_and_secure_storage() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let config_home = temp_dir("config");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        std::fs::write(
            config_home.join("settings.json"),
            serde_json::json!({
                "pluginConfigs": {
                    "toolbox@market": {
                        "mcpServers": {
                            "docs": {
                                "token": "plain-token",
                                "endpoint": "https://example.test"
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .expect("settings");
        let mut credentials =
            std::fs::File::create(config_home.join(".credentials.json")).expect("credentials");
        write!(
            credentials,
            "{}",
            serde_json::json!({
                "pluginSecrets": {
                    "toolbox@market/docs": {
                        "token": "secure-token",
                        "secretOnly": "secret"
                    }
                }
            })
        )
        .expect("write credentials");

        let merged = load_mcp_server_user_config("toolbox@market", "docs").expect("config");
        assert_eq!(
            merged.get("endpoint"),
            Some(&Value::String("https://example.test".to_string()))
        );
        assert_eq!(
            merged.get("token"),
            Some(&Value::String("secure-token".to_string()))
        );
        assert_eq!(
            merged.get("secretOnly"),
            Some(&Value::String("secret".to_string()))
        );

        let _ = std::fs::remove_dir_all(config_home);
    }
    #[tokio::test]
    async fn generated_config_matches_bun_dependency_array_and_dollar_replacement() {
        // Actual installed source dependency oracle: plugin-panel-0914/mcpb-config-oracle.ts.
        let manifest = serde_json::json!({"name":"test","server":{"mcp_config":{"command":"${__dirname}/server","args":["${user_config.items}","${user_config.flag}","prefix:${user_config.text}"],"env":{"X":"${user_config.text}"}}},"user_config":{"items":{"type":"string","multiple":true},"flag":{"type":"boolean","default":true},"text":{"type":"string"}}});
        let config = serde_json::json!({"items":["a","b"],"flag":false,"text":"$&$$"});
        let result = generate_mcp_config(
            &manifest,
            std::path::Path::new("/fixture"),
            config.as_object().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            serde_json::json!({"command":"/fixture/server","args":["a","b","false","prefix:${user_config.text}$"],"env":{"X":"${user_config.text}$"}})
        );
        let mut missing = manifest;
        missing["user_config"] = serde_json::json!({"missing":{"required":true,"default":"value"}});
        assert_eq!(
            generate_mcp_config(
                &missing,
                std::path::Path::new("/fixture"),
                &UserConfigValues::new()
            )
            .await
            .unwrap_err()
            .to_string(),
            "Failed to generate MCP server configuration from manifest \"test\""
        );
    }

    #[tokio::test]
    async fn extraction_skips_directory_records_preserves_executable_and_text_decoder() {
        let root = temp_dir("extract");
        let mut entries = indexmap::IndexMap::new();
        entries.insert("bin/".into(), Vec::new());
        entries.insert("bin/tool".into(), b"binary".to_vec());
        entries.insert("text.txt".into(), vec![0xef, 0xbb, 0xbf, b'A', 0xff]);
        let modes = std::collections::HashMap::from([("bin/tool".into(), 0o100755)]);
        let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured_progress = progress.clone();
        let callback = move |line: &str| {
            captured_progress.lock().unwrap().push(line.to_string());
            Ok(())
        };
        extract_mcpb_contents(&entries, &root, &modes, Some(&callback))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("text.txt")).unwrap(),
            "A\u{fffd}"
        );
        assert_eq!(
            *progress.lock().unwrap(),
            ["Extracting files...", "Extraction complete (2 files)"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("bin/tool"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn empty_array_variable_keeps_original_but_embedded_empty_is_substituted() {
        let manifest = serde_json::json!({"name":"empty","server":{"mcp_config":{"command":"server","args":["${user_config.x}","prefix:${user_config.x}","${user_config.missing}"]}}});
        let config = serde_json::json!({"x":""});
        let result = generate_mcp_config(
            &manifest,
            std::path::Path::new("/fixture"),
            config.as_object().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            serde_json::json!({"command":"server","args":["${user_config.x}","prefix:","${user_config.missing}"]})
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cache_paths_and_local_mtime_match_original_node_join() {
        assert_eq!(
            get_mcpb_cache_dir(std::path::Path::new("/a/../b")),
            std::path::Path::new("/b/.mcpb-cache")
        );
        assert_eq!(
            mcpb_path!("/base", "/bundle.mcpb"),
            std::path::Path::new("/base/bundle.mcpb")
        );
        let root = temp_dir("cache-paths");
        let plugin_path = root.join("absent/../plugin");
        let normalized = root.join("plugin");
        std::fs::create_dir_all(&normalized).unwrap();
        let cache = get_mcpb_cache_dir(&plugin_path);
        assert!(check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        let extraction = cache.join("extracted");
        std::fs::create_dir_all(&extraction).unwrap();
        let future = (chrono::Utc::now() + chrono::Duration::minutes(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut metadata = McpbCacheMetadata {
            source: "/bundle.mcpb".into(),
            content_hash: "abc".into(),
            extracted_path: extraction.clone(),
            cached_at: future,
            last_checked: "unused".into(),
        };
        save_cache_metadata(&cache, "/bundle.mcpb", metadata.clone())
            .await
            .unwrap();
        assert!(check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        std::fs::write(normalized.join("bundle.mcpb"), b"x").unwrap();
        assert!(!check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        // jsonParse(... as McpbCacheMetadata) does not validate unused fields.
        let mut sparse = serde_json::to_value(&metadata).unwrap();
        sparse.as_object_mut().unwrap().remove("source");
        sparse.as_object_mut().unwrap().remove("lastChecked");
        std::fs::write(
            get_metadata_path(&cache, "/bundle.mcpb"),
            sparse.to_string(),
        )
        .unwrap();
        assert!(!check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        sparse["source"] = serde_json::json!({"unused":true});
        sparse["lastChecked"] = Value::Null;
        std::fs::write(
            get_metadata_path(&cache, "/bundle.mcpb"),
            sparse.to_string(),
        )
        .unwrap();
        assert!(!check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        metadata.cached_at = "2000-01-01T00:00:00.000Z".into();
        save_cache_metadata(&cache, "/bundle.mcpb", metadata)
            .await
            .unwrap();
        assert!(check_mcpb_changed("/bundle.mcpb", &plugin_path).await);
        // Source mkdir(dirname(join(...))) normalizes away absent/.. before IO.
        let entries =
            indexmap::IndexMap::from([("absent/../normalized.txt".into(), b"content".to_vec())]);
        extract_mcpb_contents(&entries, &extraction, &Default::default(), None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(extraction.join("normalized.txt")).unwrap(),
            b"content"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    /// Maps to: CC mcpbHandler.ts#loadMcpbFile, fresh/cache/invalid branches.
    /// Fixture: actual npm 2.1.2 vAny + getMcpConfigForManifest under Bun.
    #[tokio::test]
    async fn local_bundle_schema_to_config_and_cache_matches_official_bun() {
        let dir = temp_dir("schema-pipeline");
        let valid = include_bytes!(
            "../../../tests/fixtures/oracles/mcpb-schema-0915/valid.mcpb"
        );
        std::fs::write(dir.join("probe.mcpb"), valid).unwrap();
        let oracle: Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/mcpb-schema-0915/bundle-oracle.json")).unwrap();
        let mut first_path = None;
        for _ in 0..2 {
            let loaded = load_mcpb_file("probe.mcpb", &dir, "probe@test", None, None, false)
                .await
                .unwrap();
            let McpbFileResult::Loaded(loaded) = loaded else {
                panic!("no user config required")
            };
            let mut expected_manifest = oracle["manifest"].clone();
            if first_path.is_some() {
                expected_manifest["description"] = Value::String("cache-only marker".into());
            }
            assert_eq!(loaded.manifest, expected_manifest);
            let mut config = oracle["config"].clone();
            config["args"][1] = Value::String(
                loaded
                    .extracted_path
                    .join("main.py")
                    .to_string_lossy()
                    .into_owned(),
            );
            if std::env::consts::OS != "macos" {
                config["env"] = serde_json::json!({"BASE":"1"});
            }
            assert_eq!(loaded.mcp_config, config);
            assert_eq!(
                std::fs::read_to_string(loaded.extracted_path.join("main.py")).unwrap(),
                "print(\"fixture\")\n"
            );
            if let Some(first) = &first_path {
                assert_eq!(&loaded.extracted_path, first);
            }
            let mut cached_manifest = loaded.manifest;
            cached_manifest["description"] = Value::String("cache-only marker".into());
            std::fs::write(
                loaded.extracted_path.join("manifest.json"),
                cached_manifest.to_string(),
            )
            .unwrap();
            first_path = Some(loaded.extracted_path);
        }
        let invalid = include_bytes!(
            "../../../tests/fixtures/oracles/mcpb-schema-0915/invalid.mcpb"
        );
        std::fs::write(dir.join("invalid.mcpb"), invalid).unwrap();
        let error = load_mcpb_file("invalid.mcpb", &dir, "probe@test", None, None, false)
            .await
            .err()
            .expect("strict schema rejects before extraction");
        assert_eq!(
            error.to_string(),
            "Invalid manifest: Unrecognized key(s) in object: 'unknown'"
        );
        assert!(
            !get_mcpb_cache_dir(&dir)
                .join(generate_content_hash(invalid))
                .exists()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn valid_manifest_with_missing_configuration_reaches_dialog_result() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let dir = temp_dir("schema-dialog");
        let config = dir.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        std::fs::write(dir.join("probe.mcpb"), include_bytes!("../../../tests/fixtures/oracles/mcpb-schema-0915/needs-config.mcpb")).unwrap();
        for pass in 0..2 {
            let result = load_mcpb_file("probe.mcpb", &dir, "schema-probe@test", None, None, false)
                .await
                .unwrap();
            let McpbFileResult::NeedsConfig(result) = result else {
                panic!("required token must prompt")
            };
            assert_eq!(
                result.validation_errors,
                ["Token is required but not provided"]
            );
            assert_eq!(result.config_schema["token"]["required"], true);
            assert!(result.extracted_path.join("manifest.json").exists());
            if pass == 1 {
                assert_eq!(result.manifest["description"], "cache-only marker");
            }
            let mut cached_manifest = result.manifest;
            cached_manifest["description"] = Value::String("cache-only marker".into());
            std::fs::write(
                result.extracted_path.join("manifest.json"),
                cached_manifest.to_string(),
            )
            .unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
