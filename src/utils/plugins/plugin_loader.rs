//! Maps to: CC `utils/plugins/pluginLoader.ts`.
//! Canonical plugin loading, cache materialization and memoized result assembly.

use crate::types::plugin::LoadedPlugin;
use crate::types::plugin::PluginError;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Maps to: CC pluginLoader.ts:126-128#getPluginCachePath.
pub fn get_plugin_cache_path() -> PathBuf {
    let joined = super::plugin_directories::get_plugins_directory().join("cache");
    let mut path = PathBuf::new();
    // Node join's lexical primitive, not permission canonicalization/NFC.
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if path.file_name().is_some_and(|name| name != "..") {
                    path.pop();
                } else if !path.has_root() {
                    path.push("..");
                }
            }
            component => path.push(component.as_os_str()),
        }
    }
    path
}

/// Maps to: CC pluginLoader.ts:139-165#getVersionedCachePathIn.
pub fn get_versioned_cache_path_in(base_dir: &Path, plugin_id: &str, version: &str) -> PathBuf {
    let parsed = super::plugin_identifier::parse_plugin_identifier(plugin_id);
    let marketplace = parsed
        .marketplace
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let plugin = if parsed.name.is_empty() {
        plugin_id
    } else {
        &parsed.name
    };
    // Source regexes lack the u flag: one replacement per UTF-16 code unit.
    let sanitized_marketplace: String = marketplace
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
        .collect();
    let sanitized_plugin: String = plugin
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
        .collect();
    let sanitized_version: String = version
        .encode_utf16()
        .map(|unit| {
            if unit <= 0x7f
                && ((unit as u8).is_ascii_alphanumeric()
                    || matches!(unit as u8, b'-' | b'_' | b'.'))
            {
                char::from(unit as u8)
            } else {
                '-'
            }
        })
        .collect();
    let joined = base_dir
        .join("cache")
        .join(sanitized_marketplace)
        .join(sanitized_plugin)
        .join(sanitized_version);
    let mut path = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if path.file_name().is_some_and(|name| name != "..") {
                    path.pop();
                } else if !path.has_root() {
                    path.push("..");
                }
            }
            component => path.push(component.as_os_str()),
        }
    }
    // Node join retains a trailing separator when version is the empty string
    // only if an earlier component itself ends with it; sanitized names cannot.
    path
}

/// Maps to: CC pluginLoader.ts:172-177#getVersionedCachePath.
pub fn get_versioned_cache_path(plugin_id: &str, version: &str) -> PathBuf {
    get_versioned_cache_path_in(
        &super::plugin_directories::get_plugins_directory(),
        plugin_id,
        version,
    )
}

/// Maps to: CC `utils/plugins/pluginLoader.ts:1888-2090#loadPluginsFromMarketplaces`.
pub async fn load_plugins_from_marketplaces(
    cache_only: bool,
) -> (Vec<LoadedPlugin>, Vec<PluginError>) {
    use super::marketplace_helpers::*;
    use super::marketplace_manager::*;
    let settings = crate::utils::settings::get_initial_settings();
    let enabled = merged_enabled_plugins_with_add_dir(&settings);
    let entries: Vec<(String, Value)> = enabled
        .as_ref()
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(id, _)| {
            crate::utils::zod::safe_parse(
                super::schemas::plugin_id_schema(),
                &Value::String((*id).clone()),
            )
            .is_ok()
                && super::plugin_identifier::parse_plugin_identifier(id)
                    .marketplace
                    .as_deref()
                    != Some("builtin")
        })
        .map(|(id, value)| (id.clone(), value.clone()))
        .collect();
    let known = load_known_marketplaces_config_safe().await;
    let allowlist = get_strict_known_marketplaces();
    let blocklist = get_blocked_marketplaces();
    let active_policy =
        allowlist.is_some() || blocklist.as_ref().is_some_and(|list| !list.is_empty());
    let mut names = Vec::new();
    for (id, _) in &entries {
        if let Some(name) = super::plugin_identifier::parse_plugin_identifier(id).marketplace {
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let catalogs: std::collections::HashMap<_, _> =
        futures::future::join_all(names.into_iter().map(|name| async move {
            let catalog = get_marketplace_cache_only(&name).await;
            (name, catalog)
        }))
        .await
        .into_iter()
        .collect();
    let installed = super::installed_plugins_manager::get_in_memory_installed_plugins()
        .lock()
        .unwrap()
        .clone();
    // JS callbacks share errorsOut; retain insertion-at-effect order across awaits.
    let errors = std::sync::Mutex::new(Vec::new());
    let results = futures::future::join_all(entries.iter().map(|(id, enabled)| {
        let known = &known;
        let catalogs = &catalogs;
        let installed = &installed;
        let allowlist = &allowlist;
        let errors = &errors;
        async move {
            let parsed = super::plugin_identifier::parse_plugin_identifier(id);
            let name = parsed.name;
            let marketplace = parsed.marketplace.expect("validated marketplace ID");
            let config = known.get(&marketplace);
            if config.is_none() && active_policy {
                errors
                    .lock()
                    .unwrap()
                    .push(PluginError::MarketplaceBlockedByPolicy {
                        source: id.clone(),
                        plugin: Some(name),
                        marketplace,
                        blocked_by_blocklist: Some(allowlist.is_none()),
                        allowed_sources: allowlist
                            .iter()
                            .flatten()
                            .map(format_source_for_display)
                            .collect(),
                    });
                return None;
            }
            if let Some(config) = config {
                if !is_source_allowed_by_policy(&config["source"]) {
                    let blocked = is_source_in_blocklist(&config["source"]);
                    errors
                        .lock()
                        .unwrap()
                        .push(PluginError::MarketplaceBlockedByPolicy {
                            source: id.clone(),
                            plugin: Some(name),
                            marketplace,
                            blocked_by_blocklist: Some(blocked),
                            allowed_sources: if blocked {
                                vec![]
                            } else {
                                get_strict_known_marketplaces()
                                    .into_iter()
                                    .flatten()
                                    .map(|s| format_source_for_display(&s))
                                    .collect()
                            },
                        });
                    return None;
                }
            }
            let result =
                if let (Some(Some(catalog)), Some(config)) = (catalogs.get(&marketplace), config) {
                    catalog["plugins"]
                        .as_array()
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|entry| entry["name"].as_str() == Some(&name))
                        })
                        .map(|entry| MarketplacePluginMetadata {
                            entry: entry.clone(),
                            marketplace_install_location: config.get("installLocation").cloned(),
                        })
                } else {
                    get_plugin_by_id_cache_only(id).await
                };
            let Some(result) = result else {
                errors.lock().unwrap().push(PluginError::PluginNotFound {
                    source: id.clone(),
                    plugin_id: name,
                    marketplace,
                });
                return None;
            };
            if !cache_only {
                let version = installed["plugins"]
                    .get(id)
                    .and_then(Value::as_array)
                    .and_then(|v| v.first())
                    .and_then(|v| v["version"].as_str());
                return match load_plugin_from_marketplace_entry(
                    &result.entry,
                    result.marketplace_install_location.as_ref(),
                    id,
                    enabled == &Value::Bool(true),
                    version,
                )
                .await
                {
                    Ok((plugin, extra)) => {
                        errors.lock().unwrap().extend(extra);
                        plugin
                    }
                    Err(error) => {
                        errors.lock().unwrap().push(PluginError::GenericError {
                            source: id.clone(),
                            plugin: Some(name),
                            error: error.to_string(),
                        });
                        None
                    }
                };
            }
            let entry = parse_marketplace_plugin_entry(&result.entry)
                .expect("catalog schema validates entry");
            let install = installed["plugins"]
                .get(id)
                .and_then(Value::as_array)
                .and_then(|v| v.first())
                .and_then(|v| v["installPath"].as_str())
                .map(PathBuf::from);
            let (plugin, leaf_errors) = load_plugin_from_marketplace_entry_cache_only(
                &entry,
                result.marketplace_install_location.as_ref(),
                id,
                enabled == &Value::Bool(true),
                install.as_ref(),
            )
            .await;
            errors.lock().unwrap().extend(leaf_errors);
            plugin
        }
    }))
    .await;
    (
        results.into_iter().flatten().collect(),
        errors.into_inner().unwrap(),
    )
}

/// Maps to: CC `utils/plugins/pluginLoader.ts:2098-2168#loadPluginFromMarketplaceEntryCacheOnly`.
/// Partial shared tail: the existing manifest/component loader remains synchronous;
/// ZIP extraction is an explicit unavailable operation, not a successful load.
pub(crate) async fn load_plugin_from_marketplace_entry_cache_only(
    entry: &MarketplacePluginEntry,
    marketplace_install_location: Option<&Value>,
    plugin_id: &str,
    enabled: bool,
    install_path: Option<&PathBuf>,
) -> (Option<LoadedPlugin>, Vec<PluginError>) {
    let mut plugin_path = if let Some(source_path) = entry.source.as_str() {
        // Only the local-source branch consumes the unchecked location. In the
        // original stat's type error and filesystem errors share this catch.
        let metadata = match marketplace_install_location.and_then(Value::as_str) {
            Some(location) => tokio::fs::metadata(location)
                .await
                .ok()
                .map(|metadata| (Path::new(location), metadata)),
            None => None,
        };
        let Some((location, metadata)) = metadata else {
            return (
                None,
                vec![PluginError::PluginCacheMiss {
                    source: plugin_id.into(),
                    plugin: entry.name.clone(),
                    install_path: marketplace_install_location.cloned(),
                }],
            );
        };
        let directory = if metadata.is_dir() {
            location.to_path_buf()
        } else {
            node_path_join(location, "..")
        };
        node_path_join(&directory, source_path)
    } else {
        let exists = match install_path {
            Some(path) if !path.as_os_str().is_empty() => {
                tokio::fs::try_exists(path).await.unwrap_or(false)
            }
            _ => false,
        };
        if !exists {
            return (
                None,
                vec![PluginError::PluginCacheMiss {
                    source: plugin_id.into(),
                    plugin: entry.name.clone(),
                    install_path: Some(Value::String(
                        install_path
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "(not recorded)".into()),
                    )),
                }],
            );
        }
        install_path.expect("existing path").clone()
    };
    if super::zip_cache::is_plugin_zip_cache_enabled()
        && plugin_path.to_string_lossy().ends_with(".zip")
    {
        let extraction = async {
            let root = super::zip_cache::get_session_plugin_cache_path().await?;
            let target = root.join(sanitize_cache_name(plugin_id, true));
            super::zip_cache::extract_zip_to_directory(&plugin_path, &target).await?;
            Ok::<_, anyhow::Error>(target)
        }
        .await;
        match extraction {
            Ok(target) => plugin_path = target,
            Err(_) => {
                return (
                    None,
                    vec![PluginError::PluginCacheMiss {
                        source: plugin_id.into(),
                        plugin: entry.name.clone(),
                        install_path: Some(Value::String(plugin_path.display().to_string())),
                    }],
                );
            }
        }
    }
    match finish_loading_plugin_from_path(&entry.raw, plugin_id, enabled, &plugin_path).await {
        Ok(result) => result,
        Err(error) => (
            None,
            vec![PluginError::GenericError {
                source: plugin_id.into(),
                plugin: Some(entry.name.clone()),
                error: error.to_string(),
            }],
        ),
    }
}

/// Maps to: CC pluginLoader.ts:183-190#getVersionedZipCachePath.
pub fn get_versioned_zip_cache_path(plugin_id: &str, version: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}.zip",
        get_versioned_cache_path(plugin_id, version).display()
    ))
}
/// Maps to: CC pluginLoader.ts:195-216#probeSeedCache.
async fn probe_seed_cache(plugin_id: &str, version: &str) -> Option<PathBuf> {
    for base in super::plugin_directories::get_plugin_seed_dirs() {
        let path = get_versioned_cache_path_in(&base, plugin_id, version);
        if let Ok(mut entries) = tokio::fs::read_dir(&path).await {
            if entries.next_entry().await.ok().flatten().is_some() {
                return Some(path);
            }
        }
    }
    None
}
/// Maps to: CC pluginLoader.ts:220-242#probeSeedCacheAnyVersion.
pub async fn probe_seed_cache_any_version(plugin_id: &str) -> Option<PathBuf> {
    for base in super::plugin_directories::get_plugin_seed_dirs() {
        let probe = get_versioned_cache_path_in(&base, plugin_id, "_");
        let parent = probe.parent()?;
        if let Ok(mut entries) = tokio::fs::read_dir(parent).await {
            let first = entries.next_entry().await.ok().flatten();
            let next = entries.next_entry().await.ok().flatten();
            if let (Some(first), None) = (first, next) {
                if let Ok(mut children) = tokio::fs::read_dir(first.path()).await {
                    if children.next_entry().await.ok().flatten().is_some() {
                        return Some(first.path());
                    }
                }
            }
        }
    }
    None
}
/// Maps to: CC pluginLoader.ts:249-259#getLegacyCachePath.
pub fn get_legacy_cache_path(name: &str) -> PathBuf {
    get_plugin_cache_path().join(sanitize_cache_name(name, false))
}
/// Maps to: CC pluginLoader.ts:266-287#resolvePluginPath.
pub async fn resolve_plugin_path(plugin_id: &str, version: Option<&str>) -> PathBuf {
    if let Some(version) = version {
        let path = get_versioned_cache_path(plugin_id, version);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return path;
        }
    }
    let parsed = super::plugin_identifier::parse_plugin_identifier(plugin_id);
    let legacy = get_legacy_cache_path(&parsed.name);
    if tokio::fs::try_exists(&legacy).await.unwrap_or(false) {
        return legacy;
    }
    version
        .map(|v| get_versioned_cache_path(plugin_id, v))
        .unwrap_or(legacy)
}
// Native UTF-16 carrier for the source non-u ASCII replacement regex.
fn sanitize_cache_name(value: &str, allow_at: bool) -> String {
    value
        .encode_utf16()
        .map(|c| {
            if c <= 127
                && ((c as u8).is_ascii_alphanumeric()
                    || matches!(c as u8, b'-' | b'_')
                    || (allow_at && c == 64))
            {
                char::from(c as u8)
            } else {
                '-'
            }
        })
        .collect()
}
/// Maps to: CC pluginLoader.ts:293-359#copyDir.
pub fn copy_dir<'a>(
    src: &'a Path,
    dest: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        tokio::fs::create_dir_all(dest).await?;
        let mut entries = tokio::fs::read_dir(src).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            names.push(entry.file_name());
        }
        names.sort_by(|a, b| {
            a.to_string_lossy()
                .encode_utf16()
                .cmp(b.to_string_lossy().encode_utf16())
        });
        for name in names {
            let from = src.join(&name);
            let to = dest.join(&name);
            let kind = tokio::fs::symlink_metadata(&from).await?.file_type();
            if kind.is_dir() {
                copy_dir(&from, &to).await?;
            } else if kind.is_symlink() {
                let raw = tokio::fs::read_link(&from).await?;
                let target = match tokio::fs::canonicalize(&from).await {
                    Ok(target) => {
                        let root = tokio::fs::canonicalize(src)
                            .await
                            .unwrap_or_else(|_| src.to_path_buf());
                        if let Ok(relative) = target.strip_prefix(&root) {
                            let new_target = dest.join(relative);
                            PathBuf::from(crate::utils::path::node_path_relative(
                                to.parent().unwrap(),
                                &new_target,
                            ))
                        } else {
                            target
                        }
                    }
                    Err(_) => raw,
                };
                #[cfg(unix)]
                tokio::fs::symlink(target, &to).await?;
                #[cfg(windows)]
                {
                    if tokio::fs::metadata(&from).await.is_ok_and(|m| m.is_dir()) {
                        tokio::fs::symlink_dir(target, &to).await?;
                    } else {
                        tokio::fs::symlink_file(target, &to).await?;
                    }
                }
            } else if kind.is_file() {
                tokio::fs::copy(&from, &to).await?;
            }
        }
        Ok(())
    })
}
// Node rm({recursive:true,force:true}) native IO boundary, shared by source callsites.
pub(crate) async fn remove_path_force(path: &Path) -> anyhow::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(metadata) => {
            let result = if metadata.is_dir() {
                tokio::fs::remove_dir_all(path).await
            } else {
                tokio::fs::remove_file(path).await
            };
            match result {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other.map_err(Into::into),
            }
        }
    }
}
/// Maps to: CC pluginLoader.ts:365-463#copyPluginToVersionedCache.
pub async fn copy_plugin_to_versioned_cache(
    source_path: &Path,
    plugin_id: &str,
    version: &str,
    entry: Option<&Value>,
    marketplace_dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let zip = super::zip_cache::is_plugin_zip_cache_enabled();
    let path = get_versioned_cache_path(plugin_id, version);
    let zip_path = get_versioned_zip_cache_path(plugin_id, version);
    if zip {
        if tokio::fs::try_exists(&zip_path).await? {
            return Ok(zip_path);
        }
    } else if tokio::fs::try_exists(&path).await? {
        if tokio::fs::read_dir(&path)
            .await?
            .next_entry()
            .await?
            .is_some()
        {
            return Ok(path);
        }
        tokio::fs::remove_dir(&path).await?;
    }
    if let Some(seed) = probe_seed_cache(plugin_id, version).await {
        return Ok(seed);
    }
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    if let (Some(local), Some(base)) = (entry.and_then(|e| e["source"].as_str()), marketplace_dir) {
        let source = super::plugin_installation_helpers::validate_path_within_base(base, local)?;
        copy_dir(&source, &path).await?;
    } else {
        copy_dir(source_path, &path).await?;
    }
    remove_path_force(&path.join(".git")).await?;
    if tokio::fs::read_dir(&path)
        .await?
        .next_entry()
        .await?
        .is_none()
    {
        anyhow::bail!(
            "Failed to copy plugin {plugin_id} to versioned cache: destination is empty after copy"
        );
    }
    if zip {
        super::zip_cache::convert_directory_to_zip_in_place(&path, &zip_path).await?;
        Ok(zip_path)
    } else {
        Ok(path)
    }
}
/// Maps to: CC pluginLoader.ts:470-487#validateGitUrl.
fn validate_git_url(url: &str) -> anyhow::Result<&str> {
    if regress::Regex::new(r"^git@[a-zA-Z0-9.-]+:")
        .unwrap()
        .find(url)
        .is_some()
        || url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "file"))
    {
        Ok(url)
    } else {
        anyhow::bail!("Invalid git URL: {url}")
    }
}
// Native options carrier; all processes use the existing execFileNoThrow owner.
async fn plugin_exec(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    crate::utils::exec_file_no_throw::exec_file_no_throw_with_cwd_options(
        program,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        crate::utils::exec_file_no_throw::ExecFileWithCwdOptions {
            cwd,
            ..Default::default()
        },
    )
    .await
}
/// Maps to: CC pluginLoader.ts:492-523#installFromNpm.
pub async fn install_from_npm(
    package: &str,
    target: &Path,
    registry: Option<&str>,
    version: Option<&str>,
) -> anyhow::Result<()> {
    let cache = super::plugin_directories::get_plugins_directory().join("npm-cache");
    tokio::fs::create_dir_all(&cache).await?;
    let path = cache.join("node_modules").join(package);
    if !tokio::fs::try_exists(&path).await? {
        let spec = version
            .filter(|s| !s.is_empty())
            .map(|v| format!("{package}@{v}"))
            .unwrap_or_else(|| package.into());
        let mut args = vec![
            "install".into(),
            spec,
            "--prefix".into(),
            cache.display().to_string(),
        ];
        if let Some(registry) = registry.filter(|s| !s.is_empty()) {
            args.extend(["--registry".into(), registry.into()]);
        }
        let result = plugin_exec("npm", &args, None).await;
        if result.code != 0 {
            anyhow::bail!("Failed to install npm package: {}", result.stderr);
        }
    }
    copy_dir(&path, target).await
}
/// Maps to: CC pluginLoader.ts:534-640#gitClone.
pub async fn git_clone(
    url: &str,
    target: &Path,
    reference: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<()> {
    use super::fetch_telemetry::*;
    let started = std::time::Instant::now();
    let git = crate::utils::git::git_exe().to_string_lossy().into_owned();
    let mut args = vec![
        "clone".into(),
        "--depth".into(),
        "1".into(),
        "--recurse-submodules".into(),
        "--shallow-submodules".into(),
    ];
    if let Some(reference) = reference.filter(|r| !r.is_empty()) {
        args.extend(["--branch".into(), reference.into()]);
    }
    if sha.is_some_and(|s| !s.is_empty()) {
        args.push("--no-checkout".into());
    }
    args.extend([url.into(), target.display().to_string()]);
    let result = async {
        let cloned = plugin_exec(
            &git,
            &args,
            Some(
                &std::env::current_dir()
                    .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd()),
            ),
        )
        .await;
        if cloned.code != 0 {
            return Err((
                format!("Failed to clone repository: {}", cloned.stderr),
                cloned.stderr,
            ));
        }
        if let Some(sha) = sha.filter(|s| !s.is_empty()) {
            let fetched = plugin_exec(
                &git,
                &[
                    "fetch".into(),
                    "--depth".into(),
                    "1".into(),
                    "origin".into(),
                    sha.into(),
                ],
                Some(target),
            )
            .await;
            if fetched.code != 0 {
                let full =
                    plugin_exec(&git, &["fetch".into(), "--unshallow".into()], Some(target)).await;
                if full.code != 0 {
                    return Err((
                        format!("Failed to fetch commit {sha}: {}", full.stderr),
                        full.stderr,
                    ));
                }
            }
            let checkout = plugin_exec(&git, &["checkout".into(), sha.into()], Some(target)).await;
            if checkout.code != 0 {
                return Err((
                    format!("Failed to checkout commit {sha}: {}", checkout.stderr),
                    checkout.stderr,
                ));
            }
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            log_plugin_fetch(
                PluginFetchSource::PluginClone,
                Some(url),
                PluginFetchOutcome::Success,
                started.elapsed().as_secs_f64() * 1000.,
                None,
            );
            Ok(())
        }
        Err((message, stderr)) => {
            log_plugin_fetch(
                PluginFetchSource::PluginClone,
                Some(url),
                PluginFetchOutcome::Failure,
                started.elapsed().as_secs_f64() * 1000.,
                Some(classify_fetch_error(&stderr)),
            );
            Err(anyhow::anyhow!(message))
        }
    }
}
/// Maps to: CC pluginLoader.ts:645-656#installFromGit.
async fn install_from_git(
    url: &str,
    target: &Path,
    reference: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<()> {
    validate_git_url(url)?;
    git_clone(url, target, reference, sha).await
}
/// Maps to: CC pluginLoader.ts:662-680#installFromGitHub.
async fn install_from_github(
    repo: &str,
    target: &Path,
    reference: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<()> {
    if regress::Regex::new(r"^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+$")
        .unwrap()
        .find(repo)
        .is_none()
    {
        anyhow::bail!("Invalid GitHub repository format: {repo}");
    }
    install_from_git(&resolve_git_subdir_url(repo)?, target, reference, sha).await
}
/// Maps to: CC pluginLoader.ts:686-702#resolveGitSubdirUrl.
fn resolve_git_subdir_url(url: &str) -> anyhow::Result<String> {
    if regress::Regex::new(r"^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+$")
        .unwrap()
        .find(url)
        .is_some()
    {
        Ok(
            if crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref(),
            ) {
                format!("https://github.com/{url}.git")
            } else {
                format!("git@github.com:{url}.git")
            },
        )
    } else {
        validate_git_url(url).map(str::to_owned)
    }
}
/// Maps to: CC pluginLoader.ts:718-850#installFromGitSubdir.
pub async fn install_from_git_subdir(
    url: &str,
    target: &Path,
    subdir: &str,
    reference: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if !super::git_availability::check_git_available().await {
        anyhow::bail!(
            "Git is required for git-subdir plugins. Install Git 2.25 or later and try again."
        );
    }
    let git = crate::utils::git::git_exe().to_string_lossy().into_owned();
    let url = resolve_git_subdir_url(url)?;
    let clone_dir = PathBuf::from(format!("{}.clone", target.display()));
    let mut args = vec![
        "clone".into(),
        "--depth".into(),
        "1".into(),
        "--filter=tree:0".into(),
        "--no-checkout".into(),
    ];
    if let Some(reference) = reference.filter(|s| !s.is_empty()) {
        args.extend(["--branch".into(), reference.into()]);
    }
    args.extend([url, clone_dir.display().to_string()]);
    let cloned = plugin_exec(
        &git,
        &args,
        Some(
            &std::env::current_dir()
                .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd()),
        ),
    )
    .await;
    if cloned.code != 0 {
        anyhow::bail!(
            "Failed to clone repository for git-subdir: {}",
            cloned.stderr
        );
    }
    let result = async {
        let sparse = plugin_exec(
            &git,
            &[
                "sparse-checkout".into(),
                "set".into(),
                "--cone".into(),
                "--".into(),
                subdir.into(),
            ],
            Some(&clone_dir),
        )
        .await;
        if sparse.code != 0 {
            anyhow::bail!("Failed to configure sparse-checkout: {}", sparse.stderr);
        }
        let resolved = if let Some(sha) = sha.filter(|s| !s.is_empty()) {
            let fetch = plugin_exec(
                &git,
                &[
                    "fetch".into(),
                    "--depth".into(),
                    "1".into(),
                    "origin".into(),
                    sha.into(),
                ],
                Some(&clone_dir),
            )
            .await;
            if fetch.code != 0 {
                let full = plugin_exec(
                    &git,
                    &["fetch".into(), "--unshallow".into()],
                    Some(&clone_dir),
                )
                .await;
                if full.code != 0 {
                    anyhow::bail!("Failed to fetch commit {sha}: {}", full.stderr);
                }
            }
            let checkout =
                plugin_exec(&git, &["checkout".into(), sha.into()], Some(&clone_dir)).await;
            if checkout.code != 0 {
                anyhow::bail!("Failed to checkout commit {sha}: {}", checkout.stderr);
            }
            Some(sha.into())
        } else {
            let checkout_args = ["checkout".into(), "HEAD".into()];
            let head_args = ["rev-parse".into(), "HEAD".into()];
            let (checkout, head) = tokio::join!(
                plugin_exec(&git, &checkout_args, Some(&clone_dir)),
                plugin_exec(&git, &head_args, Some(&clone_dir))
            );
            if checkout.code != 0 {
                anyhow::bail!("Failed to checkout repository: {}", checkout.stderr);
            }
            if head.code == 0 {
                Some(head.stdout.trim().into())
            } else {
                None
            }
        };
        let selected =
            super::plugin_installation_helpers::validate_path_within_base(&clone_dir, subdir)?;
        tokio::fs::rename(&selected, target).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("Subdirectory \"{subdir}\" not found in repository")
            } else {
                e.into()
            }
        })?;
        Ok(resolved)
    }
    .await;
    remove_path_force(&clone_dir).await?;
    result
}
/// Maps to: CC pluginLoader.ts:856-867#installFromLocal.
async fn install_from_local(source: &Path, target: &Path) -> anyhow::Result<()> {
    if !tokio::fs::try_exists(source).await? {
        anyhow::bail!("Local plugin path does not exist: {}", source.display());
    }
    copy_dir(source, target).await?;
    remove_path_force(&target.join(".git")).await
}
/// Maps to: CC pluginLoader.ts:873-904#generateTemporaryCacheNameForPlugin.
pub fn generate_temporary_cache_name_for_plugin(source: &Value) -> String {
    let prefix = if source.is_string() {
        "local"
    } else {
        match source["source"].as_str() {
            Some("npm") => "npm",
            Some("pip") => "pip",
            Some("github") => "github",
            Some("url") => "git",
            Some("git-subdir") => "subdir",
            _ => "unknown",
        }
    };
    // Math.random's opaque nonce is represented by OS randomness. Preserve
    // the source radix-36 alphabet and exact temp_<kind>_<ms>_<nonce> shape.
    let mut nonce = String::new();
    while nonce.len() < 6 {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).expect("plugin temporary name entropy");
        for byte in random {
            if byte < 252 {
                nonce.push(char::from_digit(u32::from(byte % 36), 36).unwrap());
                if nonce.len() == 6 {
                    break;
                }
            }
        }
    }
    format!(
        "temp_{prefix}_{}_{nonce}",
        chrono::Utc::now().timestamp_millis()
    )
}
/// Maps to: CC pluginLoader.ts:911-1100#cachePlugin return value.
#[derive(Clone, Debug)]
pub struct CachedPlugin {
    pub path: PathBuf,
    pub manifest: super::schemas::PluginManifest,
    pub git_commit_sha: Option<String>,
}
/// Maps to: CC pluginLoader.ts:911-1100#cachePlugin.
pub async fn cache_plugin(
    source: &Value,
    manifest: Option<&super::schemas::PluginManifest>,
) -> anyhow::Result<CachedPlugin> {
    let cache = get_plugin_cache_path();
    tokio::fs::create_dir_all(&cache).await?;
    let temp_name = generate_temporary_cache_name_for_plugin(source);
    let temp = cache.join(&temp_name);
    let install = async {
        if let Some(path) = source.as_str() {
            install_from_local(Path::new(path), &temp).await?;
            return Ok(None);
        }
        let reference = source["ref"].as_str();
        let sha = source["sha"].as_str();
        match source["source"].as_str() {
            Some("npm") => {
                install_from_npm(
                    source["package"].as_str().unwrap_or(""),
                    &temp,
                    source["registry"].as_str(),
                    source["version"].as_str(),
                )
                .await?
            }
            Some("github") => {
                install_from_github(source["repo"].as_str().unwrap_or(""), &temp, reference, sha)
                    .await?
            }
            Some("url") => {
                install_from_git(source["url"].as_str().unwrap_or(""), &temp, reference, sha)
                    .await?
            }
            Some("git-subdir") => {
                return install_from_git_subdir(
                    source["url"].as_str().unwrap_or(""),
                    &temp,
                    source["path"].as_str().unwrap_or(""),
                    reference,
                    sha,
                )
                .await;
            }
            Some("pip") => anyhow::bail!("Python package plugins are not yet supported"),
            _ => anyhow::bail!("Unsupported plugin source type"),
        };
        Ok(None)
    }
    .await;
    let sha = match install {
        Ok(sha) => sha,
        Err(error) => {
            if tokio::fs::try_exists(&temp).await.unwrap_or(false) {
                if let Err(cleanup) = remove_path_force(&temp).await {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Failed to clean up installation: {cleanup}"
                    ));
                }
            }
            return Err(error);
        }
    };
    let standard = temp.join(".claude-plugin/plugin.json");
    let legacy = temp.join("plugin.json");
    let manifest_path = if tokio::fs::try_exists(&standard).await? {
        Some(standard)
    } else if tokio::fs::try_exists(&legacy).await? {
        Some(legacy)
    } else {
        None
    };
    let manifest = if let Some(path) = manifest_path {
        read_validated_manifest(&path, None).await?
    } else {
        manifest
            .cloned()
            .unwrap_or_else(|| super::schemas::PluginManifest {
                name: temp_name,
                description: Some(format!(
                    "Plugin cached from {}",
                    source
                        .as_str()
                        .unwrap_or_else(|| source["source"].as_str().unwrap_or("undefined"))
                )),
                ..Default::default()
            })
    };
    let final_path = cache.join(sanitize_cache_name(&manifest.name, false));
    remove_path_force(&final_path).await?;
    tokio::fs::rename(temp, &final_path).await?;
    Ok(CachedPlugin {
        path: final_path,
        manifest,
        git_commit_sha: sha,
    })
}
// IO/error adapter for the identical manifest parser expressions in cachePlugin
// and loadPluginManifest. Their user-visible source prefixes remain separate.
async fn read_validated_manifest(
    path: &Path,
    named: Option<&str>,
) -> anyhow::Result<super::schemas::PluginManifest> {
    let prefix = named
        .map(|name| format!("Plugin {name}"))
        .unwrap_or_else(|| "Plugin".into());
    let delimiter = if named.is_some() { "\n\n" } else { " " };
    let value = async {
        let text = tokio::fs::read_to_string(path).await?;
        let value = crate::utils::slow_operations::json_parse(&text)?.to_json();
        Ok::<_, anyhow::Error>(value)
    }
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "{prefix} has a corrupt manifest file at {}.{delimiter}JSON parse error: {e}",
            path.display()
        )
    })?;
    let value = crate::utils::zod::safe_parse(super::schemas::plugin_manifest_schema(), &value)
        .map_err(|errors| {
            anyhow::anyhow!(
                "{prefix} has an invalid manifest file at {}.{delimiter}Validation errors: {}",
                path.display(),
                errors
                    .issues
                    .iter()
                    .map(|issue| {
                        let at = issue
                            .path
                            .iter()
                            .map(|part| match part {
                                crate::utils::zod::PathSegment::Key(key) => key.clone(),
                                crate::utils::zod::PathSegment::Index(index) => index.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(".");
                        if named.is_some() && at.is_empty() {
                            issue.message.clone()
                        } else {
                            format!("{at}: {}", issue.message)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    Ok(serde_json::from_value(value)?)
}
/// Maps to: CC pluginLoader.ts:1147-1220#loadPluginManifest.
pub async fn load_plugin_manifest(
    path: &Path,
    name: &str,
    source: &str,
) -> anyhow::Result<super::schemas::PluginManifest> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(super::schemas::PluginManifest {
            name: name.into(),
            description: Some(format!("Plugin from {source}")),
            ..Default::default()
        });
    }
    read_validated_manifest(path, Some(name)).await
}

/// Maps to: CC pluginLoader.ts:1224-1243#loadPluginHooks.
async fn load_plugin_hooks(path: &Path, name: &str) -> anyhow::Result<Value> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        anyhow::bail!(
            "Hooks file not found at {} for plugin {name}. If the manifest declares hooks, the file must exist.",
            path.display()
        );
    }
    let text = tokio::fs::read_to_string(path).await?;
    let raw = crate::utils::slow_operations::json_parse(&text)?.to_json();
    let validated = crate::utils::zod::safe_parse(super::schemas::plugin_hooks_schema(), &raw)
        .map_err(|error| anyhow::anyhow!(error.message()))?;
    Ok(validated["hooks"].clone())
}
/// Maps to: CC pluginLoader.ts:1265-1312#validatePluginPaths.
async fn validate_plugin_paths(
    paths: &[String],
    root: &Path,
    name: &str,
    source: &str,
    component: crate::types::plugin::PluginComponent,
    errors: &mut Vec<PluginError>,
) -> Vec<PathBuf> {
    let checks = futures::future::join_all(paths.iter().map(|path| async move {
        let path = node_path_join(root, path);
        let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
        (path, exists)
    }))
    .await;
    let mut found = Vec::new();
    for (path, exists) in checks {
        if exists {
            found.push(path);
        } else {
            crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
                "Plugin component file not found: {} for {name}",
                path.display()
            )));
            errors.push(PluginError::PathNotFound {
                source: source.into(),
                plugin: Some(name.into()),
                path: path.display().to_string(),
                component: component.clone(),
            });
        }
    }
    found
}
/// Maps to: CC pluginLoader.ts:1348-1765#createPluginFromPath.
pub async fn create_plugin_from_path(
    plugin_path: &Path,
    source: &str,
    enabled: bool,
    fallback_name: &str,
    strict: bool,
) -> anyhow::Result<(LoadedPlugin, Vec<PluginError>)> {
    use crate::types::plugin::PluginComponent;
    let manifest = load_plugin_manifest(
        &plugin_path.join(".claude-plugin/plugin.json"),
        fallback_name,
        source,
    )
    .await?;
    let mut errors = Vec::new();
    let mut plugin = LoadedPlugin {
        name: manifest.name.clone(),
        manifest: manifest.clone(),
        path: plugin_path.into(),
        source: source.into(),
        repository: source.into(),
        enabled,
        ..Default::default()
    };
    let (commands, agents, skills, styles) = tokio::join!(
        async {
            manifest.commands.is_none()
                && tokio::fs::try_exists(plugin_path.join("commands"))
                    .await
                    .unwrap_or(false)
        },
        async {
            manifest.agents.is_none()
                && tokio::fs::try_exists(plugin_path.join("agents"))
                    .await
                    .unwrap_or(false)
        },
        async {
            manifest.skills.is_none()
                && tokio::fs::try_exists(plugin_path.join("skills"))
                    .await
                    .unwrap_or(false)
        },
        async {
            manifest.output_styles.is_none()
                && tokio::fs::try_exists(plugin_path.join("output-styles"))
                    .await
                    .unwrap_or(false)
        }
    );
    plugin.commands_path = commands.then(|| plugin_path.join("commands"));
    plugin.agents_path = agents.then(|| plugin_path.join("agents"));
    plugin.skills_path = skills.then(|| plugin_path.join("skills"));
    plugin.output_styles_path = styles.then(|| plugin_path.join("output-styles"));
    if let Some(commands) = manifest.commands.as_ref() {
        let metadata = commands
            .as_object()
            .and_then(|m| m.values().next())
            .is_some_and(|v| {
                v.is_object() && (v.get("source").is_some() || v.get("content").is_some())
            });
        if metadata {
            let mut result = serde_json::Map::new();
            for (name, meta) in commands.as_object().unwrap() {
                if let Some(path) = meta["source"].as_str().filter(|s| !s.is_empty()) {
                    let found = validate_plugin_paths(
                        &[path.into()],
                        plugin_path,
                        &manifest.name,
                        source,
                        PluginComponent::Commands,
                        &mut errors,
                    )
                    .await;
                    if !found.is_empty() {
                        plugin.commands_paths.extend(found);
                        result.insert(name.clone(), meta.clone());
                    }
                } else if meta["content"].as_str().is_some_and(|s| !s.is_empty()) {
                    result.insert(name.clone(), meta.clone());
                }
            }
            if !result.is_empty() {
                plugin.commands_metadata = Some(Value::Object(result));
            }
        } else {
            plugin.commands_paths = validate_plugin_paths(
                &component_strings(commands),
                plugin_path,
                &manifest.name,
                source,
                PluginComponent::Commands,
                &mut errors,
            )
            .await;
        }
    }
    if let Some(paths) = &manifest.agents {
        plugin.agents_paths = validate_plugin_paths(
            paths,
            plugin_path,
            &manifest.name,
            source,
            PluginComponent::Agents,
            &mut errors,
        )
        .await;
    }
    if let Some(paths) = &manifest.skills {
        plugin.skills_paths = validate_plugin_paths(
            &component_strings(paths),
            plugin_path,
            &manifest.name,
            source,
            PluginComponent::Skills,
            &mut errors,
        )
        .await;
    }
    if let Some(paths) = &manifest.output_styles {
        plugin.output_styles_paths = validate_plugin_paths(
            &component_strings(paths),
            plugin_path,
            &manifest.name,
            source,
            PluginComponent::OutputStyles,
            &mut errors,
        )
        .await;
    }
    let mut merged = None;
    let mut loaded = std::collections::HashSet::new();
    let standard = plugin_path.join("hooks/hooks.json");
    if tokio::fs::try_exists(&standard).await.unwrap_or(false) {
        match load_plugin_hooks(&standard, &manifest.name).await {
            Ok(hooks) => {
                merged = Some(hooks);
                loaded.insert(
                    tokio::fs::canonicalize(&standard)
                        .await
                        .unwrap_or_else(|_| standard.clone()),
                );
            }
            Err(error) => errors.push(PluginError::HookLoadFailed {
                source: source.into(),
                plugin: manifest.name.clone(),
                hook_path: standard.display().to_string(),
                reason: error.to_string(),
            }),
        }
    }
    for spec in manifest
        .hooks
        .as_ref()
        .map(|h| h.as_array().cloned().unwrap_or_else(|| vec![h.clone()]))
        .unwrap_or_default()
    {
        if let Some(path) = spec.as_str() {
            let path = node_path_join(plugin_path, path);
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                errors.push(PluginError::PathNotFound {
                    source: source.into(),
                    plugin: Some(manifest.name.clone()),
                    path: path.display().to_string(),
                    component: PluginComponent::Hooks,
                });
                continue;
            }
            let normalized = tokio::fs::canonicalize(&path)
                .await
                .unwrap_or_else(|_| path.clone());
            if loaded.contains(&normalized) {
                if strict {
                    errors.push(PluginError::HookLoadFailed{source:source.into(),plugin:manifest.name.clone(),hook_path:path.display().to_string(),reason:format!("Duplicate hooks file detected: {} resolves to already-loaded file {}. The standard hooks/hooks.json is loaded automatically, so manifest.hooks should only reference additional hook files.",spec.as_str().unwrap(),normalized.display())});
                }
                continue;
            }
            match load_plugin_hooks(&path, &manifest.name).await {
                Ok(hooks) => {
                    merged = Some(merge_hooks_settings(merged, hooks));
                    loaded.insert(normalized);
                }
                Err(error) => errors.push(PluginError::HookLoadFailed {
                    source: source.into(),
                    plugin: manifest.name.clone(),
                    hook_path: path.display().to_string(),
                    reason: error.to_string(),
                }),
            }
        } else if spec.is_object() {
            merged = Some(merge_hooks_settings(merged, spec));
        }
    }
    plugin.hooks_config = merged;
    plugin.settings = load_plugin_settings(plugin_path, &manifest).await;
    Ok((plugin, errors))
}
// Typed carrier for the source Array.isArray(path) ? path : [path] expressions.
fn component_strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| value.as_str().map(|s| vec![s.into()]).unwrap_or_default())
}
/// Maps to: CC pluginLoader.ts:1788-1800#parsePluginSettings.
/// Maps to: CC pluginLoader.ts:1776-1782#PluginSettingsSchema.
/// The existing Schema value tree carries pick({agent:true}).strip() directly;
/// validation reuses the actual SettingsSchema field and zod safe_parse.
fn plugin_settings_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod::Schema;
        let shape = match crate::utils::settings::types::settings_schema() {
            Schema::Object(shape)
            | Schema::PassthroughObject(shape)
            | Schema::StrictObject(shape) => shape,
            _ => unreachable!("SettingsSchema is an object"),
        };
        Schema::Object(
            shape
                .iter()
                .filter(|(key, _)| *key == "agent")
                .cloned()
                .collect(),
        )
    })
}
/// Maps to: CC pluginLoader.ts:1788-1800#parsePluginSettings.
fn parse_plugin_settings(raw: &Value) -> Option<Value> {
    let data = crate::utils::zod::safe_parse(plugin_settings_schema(), raw).ok()?;
    data.as_object()
        .is_some_and(|map| !map.is_empty())
        .then_some(data)
}
/// Maps to: CC pluginLoader.ts:1807-1849#loadPluginSettings.
async fn load_plugin_settings(
    path: &Path,
    manifest: &super::schemas::PluginManifest,
) -> Option<Value> {
    let warn = |error: String| {
        crate::utils::debug::log_for_debugging_with_level(
            &format!(
                "Failed to parse settings.json for plugin {}: {error}",
                manifest.name
            ),
            crate::utils::debug::DebugLogLevel::Warn,
        )
    };
    match tokio::fs::read_to_string(path.join("settings.json")).await {
        Ok(content) => match crate::utils::slow_operations::json_parse(&content) {
            Ok(parsed) => {
                if let Some(settings) = parse_plugin_settings(&parsed.to_json()) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Loaded settings from settings.json for plugin {}",
                        manifest.name
                    ));
                    return Some(settings);
                }
            }
            Err(error) => warn(error.to_string()),
        },
        Err(error) => {
            if !crate::utils::errors::is_fs_inaccessible(&error) {
                warn(error.to_string());
            }
        }
    }
    let settings = manifest.settings.as_ref().and_then(parse_plugin_settings);
    if settings.is_some() {
        crate::utils::debug::log_for_debugging(&format!(
            "Loaded settings from manifest for plugin {}",
            manifest.name
        ));
    }
    settings
}
/// Maps to: CC pluginLoader.ts:1854-1876#mergeHooksSettings.
fn merge_hooks_settings(base: Option<Value>, additional: Value) -> Value {
    let Some(Value::Object(mut base)) = base else {
        return additional;
    };
    if let Value::Object(extra) = additional {
        for (event, matchers) in extra {
            if let Some(Value::Array(existing)) = base.get_mut(&event) {
                if let Value::Array(mut other) = matchers {
                    existing.append(&mut other);
                }
            } else {
                base.insert(event, matchers);
            }
        }
    }
    Value::Object(base)
}
/// Maps to: CC pluginLoader.ts:2420-2919#finishLoadingPluginFromPath.
async fn finish_loading_plugin_from_path(
    entry: &Value,
    id: &str,
    enabled: bool,
    path: &Path,
) -> anyhow::Result<(Option<LoadedPlugin>, Vec<PluginError>)> {
    use crate::types::plugin::PluginComponent;
    let name = entry["name"].as_str().unwrap_or_default();
    let has_manifest = tokio::fs::try_exists(path.join(".claude-plugin/plugin.json"))
        .await
        .unwrap_or(false);
    let (mut plugin, mut errors) = create_plugin_from_path(
        path,
        id,
        enabled,
        name,
        entry["strict"].as_bool().unwrap_or(true),
    )
    .await?;
    plugin.sha = entry["source"]["sha"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if !has_manifest {
        plugin.manifest = serde_json::from_value(entry.clone())?;
        plugin.name = plugin.manifest.name.clone();
    } else if !entry["strict"].as_bool().unwrap_or(false)
        && ["commands", "agents", "skills", "hooks", "outputStyles"]
            .iter()
            .any(|key| entry.get(*key).is_some())
    {
        return Ok((
            None,
            vec![PluginError::GenericError {
                source: id.into(),
                plugin: None,
                error: format!(
                    "Plugin {name} has conflicting manifests: both plugin.json and marketplace entry specify components. Set strict: true in marketplace entry or remove component specs from one location."
                ),
            }],
        ));
    }
    if let Some(commands) = entry.get("commands") {
        let metadata = commands
            .as_object()
            .and_then(|m| m.values().next())
            .is_some_and(|v| {
                v.is_object() && (v.get("source").is_some() || v.get("content").is_some())
            });
        if metadata {
            let mut result = if has_manifest {
                plugin
                    .commands_metadata
                    .as_ref()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default()
            } else {
                Default::default()
            };
            let mut valid = Vec::new();
            for (command, meta) in commands.as_object().unwrap() {
                if let Some(relative) = meta["source"].as_str().filter(|s| !s.is_empty()) {
                    let found = validate_plugin_paths(
                        &[relative.into()],
                        path,
                        name,
                        id,
                        PluginComponent::Commands,
                        &mut errors,
                    )
                    .await;
                    if !found.is_empty() {
                        valid.extend(found);
                        result.insert(command.clone(), meta.clone());
                    }
                }
            }
            if !valid.is_empty() {
                if has_manifest {
                    plugin.commands_paths.extend(valid);
                } else {
                    plugin.commands_paths = valid;
                }
                plugin.commands_metadata = Some(Value::Object(result));
            }
        } else {
            let valid = validate_plugin_paths(
                &component_strings(commands),
                path,
                name,
                id,
                PluginComponent::Commands,
                &mut errors,
            )
            .await;
            if !valid.is_empty() {
                if has_manifest {
                    plugin.commands_paths.extend(valid);
                } else {
                    plugin.commands_paths = valid;
                }
            }
        }
    }
    for (key, component) in [
        ("agents", PluginComponent::Agents),
        ("skills", PluginComponent::Skills),
        ("outputStyles", PluginComponent::OutputStyles),
    ] {
        if let Some(value) = entry.get(key) {
            let found = validate_plugin_paths(
                &component_strings(value),
                path,
                name,
                id,
                component,
                &mut errors,
            )
            .await;
            if !found.is_empty() {
                let target = match key {
                    "agents" => &mut plugin.agents_paths,
                    "skills" => &mut plugin.skills_paths,
                    _ => &mut plugin.output_styles_paths,
                };
                if has_manifest {
                    target.extend(found);
                } else {
                    *target = found;
                }
            }
        }
    }
    if let Some(hooks) = entry.get("hooks") {
        if has_manifest {
            let mut merged = plugin
                .hooks_config
                .take()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            if let Some(hooks) = hooks.as_object() {
                merged.extend(hooks.clone());
            }
            plugin.hooks_config = Some(Value::Object(merged));
        } else {
            plugin.hooks_config = Some(hooks.clone());
        }
    }
    Ok((Some(plugin), errors))
}
/// Maps to: CC pluginLoader.ts:2928-2991#loadSessionOnlyPlugins.
async fn load_session_only_plugins(paths: &[PathBuf]) -> (Vec<LoadedPlugin>, Vec<PluginError>) {
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let raw = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| crate::bootstrap::state::get_original_cwd())
                .join(path)
        };
        let path = node_path_join(&raw, "");
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            errors.push(PluginError::PathNotFound {
                source: format!("inline[{index}]"),
                plugin: None,
                path: path.display().to_string(),
                component: crate::types::plugin::PluginComponent::Commands,
            });
            continue;
        }
        match create_plugin_from_path(&path, &format!("{name}@inline"), true, &name, true).await {
            Ok((mut plugin, extra)) => {
                plugin.source = format!("{}@inline", plugin.name);
                plugin.repository = plugin.source.clone();
                plugins.push(plugin);
                errors.extend(extra);
            }
            Err(error) => errors.push(PluginError::GenericError {
                source: format!("inline[{index}]"),
                plugin: None,
                error: format!("Failed to load plugin: {error}"),
            }),
        }
    }
    (plugins, errors)
}
/// Source memoized Promise runtime carrier; full and cache-only remain separate.
type PluginLoadPromise = futures::future::Shared<
    futures::future::BoxFuture<
        'static,
        Result<crate::types::plugin::PluginLoadResult, std::sync::Arc<anyhow::Error>>,
    >,
>;
static FULL_PLUGIN_CACHE: std::sync::Mutex<Option<PluginLoadPromise>> = std::sync::Mutex::new(None);
static READ_PLUGIN_CACHE: std::sync::Mutex<Option<PluginLoadPromise>> = std::sync::Mutex::new(None);
/// Maps to: CC pluginLoader.ts:3096-3108#loadAllPlugins.
pub fn load_all_plugins()
-> impl std::future::Future<Output = anyhow::Result<crate::types::plugin::PluginLoadResult>> + Send
{
    use futures::FutureExt;
    let promise = {
        let mut cache = FULL_PLUGIN_CACHE.lock().unwrap();
        cache
            .get_or_insert_with(|| {
                let promise = async {
                    let result = assemble_plugin_load_result(false).await;
                    if result.is_ok() {
                        *READ_PLUGIN_CACHE.lock().unwrap() =
                            Some(futures::future::ready(result.clone()).boxed().shared());
                    }
                    result
                }
                .boxed()
                .shared();
                let worker = promise.clone();
                crate::utils::process_runtime::runtime_handle_for_detached_work()
                    .expect("plugin loading requires process lifetime runtime")
                    .spawn(async move {
                        let _ = worker.await;
                    });
                promise
            })
            .clone()
    };
    async move { promise.await.map_err(|e| anyhow::anyhow!("{e}")) }
}
/// Maps to: CC pluginLoader.ts:3137-3148#loadAllPluginsCacheOnly.
pub fn load_all_plugins_cache_only()
-> impl std::future::Future<Output = anyhow::Result<crate::types::plugin::PluginLoadResult>> + Send
{
    use futures::FutureExt;
    let promise = if crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_SYNC_PLUGIN_INSTALL")
            .as_deref(),
    ) {
        return futures::future::Either::Left(load_all_plugins());
    } else {
        let mut cache = READ_PLUGIN_CACHE.lock().unwrap();
        cache
            .get_or_insert_with(|| {
                let promise = async { assemble_plugin_load_result(true).await }
                    .boxed()
                    .shared();
                let worker = promise.clone();
                crate::utils::process_runtime::runtime_handle_for_detached_work()
                    .expect("plugin loading requires process lifetime runtime")
                    .spawn(async move {
                        let _ = worker.await;
                    });
                promise
            })
            .clone()
    };
    futures::future::Either::Right(async move { promise.await.map_err(|e| anyhow::anyhow!("{e}")) })
}
/// Maps to: CC pluginLoader.ts:3155-3210#assemblePluginLoadResult.
async fn assemble_plugin_load_result(
    cache_only: bool,
) -> Result<crate::types::plugin::PluginLoadResult, std::sync::Arc<anyhow::Error>> {
    let inline = crate::bootstrap::state::get_inline_plugins();
    let ((marketplace, mut errors), (session, session_errors)) = tokio::join!(
        load_plugins_from_marketplaces(cache_only),
        load_session_only_plugins(&inline)
    );
    let builtin = crate::plugins::builtin_plugins::get_builtin_plugins();
    let builtin = builtin
        .enabled
        .into_iter()
        .chain(builtin.disabled)
        .collect::<Vec<_>>();
    let managed = super::managed_plugins::get_managed_plugin_names();
    let (mut plugins, merge_errors) = merge_plugin_sources(PluginSources {
        session: &session,
        marketplace: &marketplace,
        builtin: &builtin,
        managed_names: managed.as_ref(),
    });
    errors.extend(session_errors);
    errors.extend(merge_errors);
    let (demoted, dep_errors) = super::dependency_resolver::verify_and_demote(&plugins);
    for plugin in &mut plugins {
        if demoted.contains(&plugin.source) {
            plugin.enabled = false;
        }
    }
    errors.extend(dep_errors);
    let enabled = plugins
        .iter()
        .filter(|p| p.enabled)
        .cloned()
        .collect::<Vec<_>>();
    cache_plugin_settings(&enabled);
    Ok(crate::types::plugin::PluginLoadResult {
        enabled,
        disabled: plugins.into_iter().filter(|p| !p.enabled).collect(),
        errors: errors.into(),
    })
}
/// Maps to: CC pluginLoader.ts:3225-3240#clearPluginCache.
pub fn clear_plugin_cache(reason: Option<&str>) {
    if let Some(reason) = reason.filter(|s| !s.is_empty()) {
        crate::utils::debug::log_for_debugging(&format!(
            "clearPluginCache: invalidating loadAllPlugins cache ({reason})"
        ));
    }
    *FULL_PLUGIN_CACHE.lock().unwrap() = None;
    *READ_PLUGIN_CACHE.lock().unwrap() = None;
    if crate::utils::settings::settings_cache::get_plugin_settings_base().is_some() {
        crate::utils::settings::settings_cache::reset_settings_cache();
    }
    crate::utils::settings::settings_cache::clear_plugin_settings_base();
}
/// Maps to: CC pluginLoader.ts:3250-3275#mergePluginSettings.
fn merge_plugin_settings(plugins: &[LoadedPlugin]) -> Option<Value> {
    let mut merged = None;
    for plugin in plugins {
        if let Some(Value::Object(settings)) = &plugin.settings {
            let base = merged.get_or_insert_with(serde_json::Map::new);
            for (key, value) in settings {
                if base.contains_key(key) {
                    crate::utils::debug::log_for_debugging(&format!(
                        "Plugin \"{}\" overrides setting \"{key}\" (previously set by another plugin)",
                        plugin.name
                    ));
                }
                base.insert(key.clone(), value.clone());
            }
        }
    }
    merged.map(Value::Object)
}
/// Maps to: CC pluginLoader.ts:3281-3296#cachePluginSettings.
pub fn cache_plugin_settings(plugins: &[LoadedPlugin]) {
    let settings = merge_plugin_settings(plugins);
    let nonempty = settings
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|m| !m.is_empty());
    crate::utils::settings::settings_cache::set_plugin_settings_base(
        settings.and_then(|v| serde_json::from_value(v).ok()),
    );
    if nonempty {
        crate::utils::settings::settings_cache::reset_settings_cache();
    }
}

/// Maps to: CC pluginLoader.ts:2191-2409#loadPluginFromMarketplaceEntry.
async fn load_plugin_from_marketplace_entry(
    entry: &Value,
    location: Option<&Value>,
    id: &str,
    enabled: bool,
    installed_version: Option<&str>,
) -> anyhow::Result<(Option<LoadedPlugin>, Vec<PluginError>)> {
    let name = entry["name"].as_str().unwrap_or_default();
    let source = &entry["source"];
    let mut path = if let Some(local) = source.as_str() {
        let location = Path::new(
            location
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Marketplace install location must be a string"))?,
        );
        let meta = tokio::fs::metadata(location).await?;
        let directory = if meta.is_dir() {
            location.to_path_buf()
        } else {
            node_path_join(location, "..")
        };
        let source_path = node_path_join(&directory, local);
        if !tokio::fs::try_exists(&source_path).await.unwrap_or(false) {
            return Ok((
                None,
                vec![PluginError::GenericError {
                    source: id.into(),
                    plugin: None,
                    error: format!(
                        "Plugin directory not found at path: {}. Check that the marketplace entry has the correct path.",
                        source_path.display()
                    ),
                }],
            ));
        }
        let manifest =
            load_plugin_manifest(&source_path.join(".claude-plugin/plugin.json"), name, local)
                .await
                .ok();
        let version = super::plugin_versioning::calculate_plugin_version(
            id,
            source,
            manifest.as_ref(),
            Some(&directory),
            entry["version"].as_str(),
            None,
        )
        .await;
        match copy_plugin_to_versioned_cache(
            &source_path,
            id,
            &version,
            Some(entry),
            Some(&directory),
        )
        .await
        {
            Ok(path) => path,
            Err(error) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "Failed to copy plugin {name} to versioned cache: {error}. Using marketplace path."
                ));
                source_path
            }
        }
    } else {
        let cached = async {
            let version = super::plugin_versioning::calculate_plugin_version(
                id,
                source,
                None,
                None,
                installed_version.or_else(|| entry["version"].as_str()),
                source["sha"].as_str(),
            )
            .await;
            let path = get_versioned_cache_path(id, &version);
            let zip = get_versioned_zip_cache_path(id, &version);
            if super::zip_cache::is_plugin_zip_cache_enabled()
                && tokio::fs::try_exists(&zip).await?
            {
                return Ok(zip);
            }
            if tokio::fs::try_exists(&path).await? {
                return Ok(path);
            }
            let mut seed = probe_seed_cache(id, &version).await;
            if seed.is_none() && version == "unknown" {
                seed = probe_seed_cache_any_version(id).await;
            }
            if let Some(seed) = seed {
                return Ok(seed);
            }
            let cached = cache_plugin(
                source,
                Some(&super::schemas::PluginManifest {
                    name: name.into(),
                    ..Default::default()
                }),
            )
            .await?;
            let actual = if version != "unknown" {
                version
            } else {
                super::plugin_versioning::calculate_plugin_version(
                    id,
                    source,
                    Some(&cached.manifest),
                    Some(&cached.path),
                    installed_version.or_else(|| entry["version"].as_str()),
                    cached.git_commit_sha.as_deref(),
                )
                .await
            };
            let path = copy_plugin_to_versioned_cache(&cached.path, id, &actual, Some(entry), None)
                .await?;
            if path != cached.path {
                remove_path_force(&cached.path).await?;
            }
            Ok::<_, anyhow::Error>(path)
        }
        .await;
        match cached {
            Ok(path) => path,
            Err(error) => {
                return Ok((
                    None,
                    vec![PluginError::GenericError {
                        source: id.into(),
                        plugin: None,
                        error: format!("Failed to download/cache plugin {name}: {error}"),
                    }],
                ));
            }
        }
    };
    if super::zip_cache::is_plugin_zip_cache_enabled() && path.to_string_lossy().ends_with(".zip") {
        let root = super::zip_cache::get_session_plugin_cache_path().await?;
        let extracted = root.join(sanitize_cache_name(id, true));
        if let Err(error) = super::zip_cache::extract_zip_to_directory(&path, &extracted).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error);
        }
        path = extracted;
    }
    finish_loading_plugin_from_path(entry, id, enabled, &path).await
}

/// Input bundle for `merge_plugin_sources`.
///
/// Maps to CC `pluginLoader.ts#mergePluginSources` source buckets.
pub struct PluginSources<'a> {
    pub session: &'a [LoadedPlugin],
    pub marketplace: &'a [LoadedPlugin],
    pub builtin: &'a [LoadedPlugin],
    pub managed_names: Option<&'a HashSet<String>>,
}

/// Merge session, marketplace, and built-in plugin sources using official
/// precedence.
///
/// Maps to: CC `utils/plugins/pluginLoader.ts#mergePluginSources`.
/// Session `--plugin-dir` plugins override installed marketplace plugins with
/// the same name, except names locked by managed policy settings are dropped
/// before the override set is computed.
pub fn merge_plugin_sources(sources: PluginSources<'_>) -> (Vec<LoadedPlugin>, Vec<PluginError>) {
    let mut errors = Vec::new();

    let session_plugins = sources
        .session
        .iter()
        .filter(|plugin| {
            if sources
                .managed_names
                .is_some_and(|managed| managed.contains(&plugin.name))
            {
                tracing::warn!(
                    plugin = %plugin.name,
                    source = %plugin.source,
                    "session plugin is blocked by managed settings"
                );
                errors.push(PluginError::GenericError {
                    source: plugin.source.clone(),
                    plugin: None,
                    error: format!(
                        "--plugin-dir copy of \"{}\" ignored: plugin is locked by managed settings",
                        plugin.name
                    ),
                });
                return false;
            }
            true
        })
        .cloned()
        .collect::<Vec<_>>();

    let session_names = session_plugins
        .iter()
        .map(|plugin| plugin.name.clone())
        .collect::<HashSet<_>>();
    let marketplace_plugins = sources
        .marketplace
        .iter()
        .filter(|plugin| {
            if session_names.contains(&plugin.name) {
                tracing::debug!(
                    plugin = %plugin.name,
                    source = %plugin.source,
                    "session plugin overrides installed plugin"
                );
                return false;
            }
            true
        })
        .cloned();

    let plugins = session_plugins
        .into_iter()
        .chain(marketplace_plugins)
        .chain(sources.builtin.iter().cloned())
        .collect();
    (plugins, errors)
}

/// Native borrowed-source DTO projection of PluginMarketplaceEntry, whose schema lives in schemas.rs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MarketplacePluginEntry {
    pub(crate) raw: Value,
    pub(crate) name: String,
    pub(crate) source: Value,
}
pub(crate) fn parse_marketplace_plugin_entry(value: &Value) -> Option<MarketplacePluginEntry> {
    Some(MarketplacePluginEntry {
        raw: value.clone(),
        name: value.get("name")?.as_str()?.into(),
        source: value.get("source").cloned().unwrap_or(Value::Null),
    })
}
/// Maps to: pluginLoader.ts#loadPluginsFromMarketplaces initial settings spread.
pub(crate) fn merged_enabled_plugins_with_add_dir(
    settings: &crate::utils::settings::types::SettingsJson,
) -> Option<Value> {
    let mut result = super::add_dir_plugin_settings::get_add_dir_enabled_plugins();
    if let Some(standard) = settings.enabled_plugins.as_ref().and_then(Value::as_object) {
        result.extend(standard.clone());
    }
    (!result.is_empty()).then_some(Value::Object(result))
}
/// Native carrier of the imported Node path.join primitive. Normalize the
/// complete concatenation, including lexical segments already present in base.
pub(crate) fn node_path_join(plugin_path: &Path, relative_path: &str) -> PathBuf {
    let mut joined = plugin_path.as_os_str().to_os_string();
    if !joined.is_empty() && !relative_path.is_empty() {
        joined.push(std::path::MAIN_SEPARATOR.to_string());
    }
    joined.push(relative_path);
    let mut out = PathBuf::new();
    for component in Path::new(&joined).components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => {
                if out.as_os_str().is_empty() {
                    out.push(std::path::MAIN_SEPARATOR.to_string());
                }
            }
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => {
                    if !out.has_root() {
                        out.push("..");
                    }
                }
            },
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}
/// A4 sync caller transport for existing commands/MCP/LSP APIs. All loading,
/// dependency checks and memoization are owned by loadAllPluginsCacheOnly above.
pub(crate) fn load_all_plugins_cache_only_from_sync() -> crate::types::plugin::PluginLoadResult {
    let result = crate::utils::process_runtime::block_on_from_sync(async {
        load_all_plugins_cache_only().await
    });
    match result {
        Some(Ok(loaded)) => loaded,
        other => crate::types::plugin::PluginLoadResult {
            errors: vec![PluginError::GenericError {
                source: "plugins".into(),
                plugin: None,
                error: match other {
                    Some(Err(error)) => error.to_string(),
                    None => "Unable to initialize plugin loading runtime".into(),
                    _ => unreachable!(),
                },
            }]
            .into(),
            ..Default::default()
        },
    }
}
#[cfg(test)]
pub(crate) fn create_plugin_from_path_for_test(
    path: &Path,
    source: &str,
    enabled: bool,
    fallback: &str,
) -> (LoadedPlugin, Vec<PluginError>) {
    let (path, source, fallback) = (path.to_path_buf(), source.to_owned(), fallback.to_owned());
    crate::utils::process_runtime::block_on_from_sync(async move {
        create_plugin_from_path(&path, &source, enabled, &fallback, true).await
    })
    .expect("test runtime")
    .expect("valid plugin fixture")
}
#[cfg(test)]
mod tests {
    use super::super::load_plugin_agents::load_plugin_agents_from_plugins;
    use super::*;
    use crate::types::plugin::PluginComponent;
    use crate::utils::plugins::schemas::PluginManifest;
    use crate::utils::settings::{
        constants::SettingSource, settings_cache, validation::SettingsWithErrors,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::fs;
    fn temp_dir(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("cometix-catalog-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }
    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn marketplace_loader_matches_official_catalog_and_installed_cache_gates() {
        // The production entrypoint publishes the process executor before
        // plugin agent traversal; a current-thread test runtime is not it.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let cache = temp_dir("cache");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &cache);
        settings_cache::reset_settings_cache();
        settings_cache::set_cached_settings_for_source(SettingSource::Policy, None);
        super::super::installed_plugins_manager::clear_installed_plugins_cache();
        let enabled_root = temp_dir("installed-enabled");
        write_file(
            &enabled_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"market-agent"}"#,
        );
        write_file(
            &enabled_root.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Review from cache\n---\nCached prompt",
        );
        let disabled_root = temp_dir("installed-disabled");
        write_file(
            &disabled_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"disabled-agent"}"#,
        );
        let missing_root = temp_dir("installed-missing").join("gone");

        let settings = crate::utils::settings::types::SettingsJson {
            enabled_plugins: Some(serde_json::json!({
                "market-agent@market": true,
                "disabled-agent@market": false,
                "builtin-one@builtin": true,
                "missing-agent@market": true,
                "legacy": true,
            })),
            ..Default::default()
        };
        // CC InstalledPluginsFileSchemaV2 requires scope/installPath; this
        // fixture now exercises the canonical object, not the old untagged DTO.
        let installed = serde_json::json!({"version":2,"plugins": {
            "market-agent@market": [{"scope":"user","installPath":enabled_root}],
            "disabled-agent@market": [{"scope":"user","installPath":disabled_root}],
            "missing-agent@market": [{"scope":"user","installPath":missing_root}]
        }});

        // Original :2005-2038 requires catalog membership independently of
        // installed_plugins.json. The old fixture pinned the invalid fallback.
        let catalog = serde_json::json!({"name":"market","owner":{"name":"Team"},"plugins":[
            {"name":"market-agent","source":{"source":"github","repo":"team/agent"}},
            {"name":"disabled-agent","source":{"source":"github","repo":"team/disabled"}},
            {"name":"missing-agent","source":{"source":"github","repo":"team/missing"}}
        ]});
        write_file(
            &cache.join(".claude-plugin/marketplace.json"),
            &catalog.to_string(),
        );
        write_file(&cache.join("known_marketplaces.json"), &serde_json::json!({"market":{
            "source":{"source":"directory","path":cache},"installLocation":cache,"lastUpdated":"2026-09-14T00:00:00.000Z"
        }}).to_string());
        write_file(
            &cache.join("installed_plugins.json"),
            &installed.to_string(),
        );
        settings_cache::set_session_settings_cache(SettingsWithErrors {
            settings,
            ..Default::default()
        });
        let (plugins, errors) = load_plugins_from_marketplaces(true).await;

        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "market-agent");
        assert_eq!(plugins[0].source, "market-agent@market");
        assert!(plugins[0].enabled);
        assert_eq!(plugins[1].name, "disabled-agent");
        assert!(!plugins[1].enabled);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].source(), "missing-agent@market");

        let agents = load_plugin_agents_from_plugins(&plugins);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_type, "market-agent:reviewer");
        assert!(
            matches!(&errors[0], PluginError::PluginCacheMiss { install_path, .. } if install_path.as_ref().and_then(Value::as_str) == Some(missing_root.to_str().unwrap()))
        );
        // Removing the catalog entry must never reuse an orphan installPath.
        write_file(
            &cache.join(".claude-plugin/marketplace.json"),
            r#"{"name":"market","owner":{"name":"Team"},"plugins":[]}"#,
        );
        let (plugins, errors) = load_plugins_from_marketplaces(true).await;
        assert!(plugins.is_empty());
        assert_eq!(errors.len(), 3);
        assert!(
            errors
                .iter()
                .all(|error| matches!(error, PluginError::PluginNotFound { .. }))
        );
        settings_cache::reset_settings_cache();
        super::super::installed_plugins_manager::clear_installed_plugins_cache();
    }

    #[tokio::test]
    async fn marketplace_loader_matches_official_unknown_source_policy_guard() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let cache = temp_dir("policy");
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &cache);
        settings_cache::reset_settings_cache();
        super::super::installed_plugins_manager::clear_installed_plugins_cache();
        let settings = crate::utils::settings::SettingsJson {
            enabled_plugins: Some(serde_json::json!({"agent@market":true})),
            ..Default::default()
        };
        settings_cache::set_session_settings_cache(SettingsWithErrors {
            settings,
            ..Default::default()
        });
        // :1920-1938 includes empty allowlist, but excludes empty blocklist.
        for (policy, blocked, by_blocklist) in [
            (
                serde_json::json!({"strictKnownMarketplaces":[]}),
                true,
                false,
            ),
            (serde_json::json!({"blockedMarketplaces":[]}), false, false),
            (
                serde_json::json!({"blockedMarketplaces":[{"source":"github","repo":"team/repo"}]}),
                true,
                true,
            ),
        ] {
            settings_cache::set_cached_settings_for_source(
                SettingSource::Policy,
                Some(serde_json::from_value(policy).unwrap()),
            );
            for raw_usable in [false, true] {
                // Original Safe config rejects missing source, while cache-only
                // lookup deliberately raw-casts and can still find this catalog.
                write_file(
                    &cache.join(".claude-plugin/marketplace.json"),
                    r#"{"name":"market","owner":{"name":"Team"},"plugins":[{"name":"agent","source":"./agent"}]}"#,
                );
                write_file(
                    &cache.join("agent/.claude-plugin/plugin.json"),
                    r#"{"name":"agent"}"#,
                );
                let raw = if raw_usable {
                    serde_json::json!({"market":{"installLocation":cache}}).to_string()
                } else {
                    "{invalid".into()
                };
                write_file(&cache.join("known_marketplaces.json"), &raw);
                let (plugins, errors) = load_plugins_from_marketplaces(true).await;
                if blocked {
                    assert!(plugins.is_empty());
                    assert_eq!(errors.len(), 1);
                    assert!(
                        matches!(&errors[0], PluginError::MarketplaceBlockedByPolicy { blocked_by_blocklist: Some(value), allowed_sources, .. } if *value == by_blocklist && allowed_sources.is_empty())
                    );
                } else if raw_usable {
                    assert_eq!(plugins.len(), 1);
                    assert!(errors.is_empty());
                } else {
                    assert!(plugins.is_empty());
                    assert_eq!(errors.len(), 1);
                    assert!(matches!(&errors[0], PluginError::PluginNotFound { .. }));
                }
            }
        }
        settings_cache::reset_settings_cache();
        super::super::installed_plugins_manager::clear_installed_plugins_cache();
    }

    #[tokio::test]
    async fn marketplace_cache_leaf_matches_official_raw_location_branching() {
        // marketplaceManager:2188-2223 can return a different first-read value
        // from the path used by its second read. pluginLoader:2108-2139 consumes
        // that value only in the local branch; external cache loading ignores it.
        let cache = temp_dir("raw-location");
        write_file(
            &cache.join(".claude-plugin/plugin.json"),
            r#"{"name":"agent"}"#,
        );
        let remote = super::parse_marketplace_plugin_entry(
            &serde_json::json!({"name":"agent","source":{"source":"github","repo":"team/agent"}}),
        )
        .unwrap();
        let local = super::parse_marketplace_plugin_entry(
            &serde_json::json!({"name":"agent","source":"./agent"}),
        )
        .unwrap();
        for location in [
            None,
            Some(Value::Null),
            Some(serde_json::json!(42)),
            Some(serde_json::json!({"x":1})),
        ] {
            let (plugin, errors) = load_plugin_from_marketplace_entry_cache_only(
                &remote,
                location.as_ref(),
                "agent@market",
                true,
                Some(&cache),
            )
            .await;
            assert!(plugin.is_some());
            assert!(errors.is_empty());
            let (plugin, errors) = load_plugin_from_marketplace_entry_cache_only(
                &local,
                location.as_ref(),
                "agent@market",
                true,
                Some(&cache),
            )
            .await;
            assert!(plugin.is_none());
            assert_eq!(
                errors,
                vec![PluginError::PluginCacheMiss {
                    source: "agent@market".into(),
                    plugin: "agent".into(),
                    install_path: location.clone()
                }]
            );
            let serialized = serde_json::to_value(&errors[0]).unwrap();
            assert_eq!(serialized.get("installPath"), location.as_ref());
            assert_eq!(
                serde_json::from_value::<PluginError>(serialized).unwrap(),
                errors[0]
            );
        }
    }

    #[test]
    fn versioned_plugin_paths_match_official_bun_utf16_regex_and_join() {
        let oracle:serde_json::Value=serde_json::from_str(include_str!("../../../tests/fixtures/oracles/plugin-installed-0914/bun-oracle.json")).unwrap();
        // Verbatim source getVersionedCachePathIn :139-165, including source
        // regex's UTF-16 code-unit replacement and Node lexical dot segments.
        for row in oracle["pathCases"].as_array().unwrap() {
            assert_eq!(
                get_versioned_cache_path_in(
                    Path::new(row["base"].as_str().unwrap()),
                    row["id"].as_str().unwrap(),
                    row["version"].as_str().unwrap()
                )
                .to_string_lossy(),
                row["path"].as_str().unwrap()
            );
        }
    }

    fn test_loaded_plugin(name: &str, source: &str, enabled: bool) -> LoadedPlugin {
        LoadedPlugin {
            name: name.to_string(),
            source: source.to_string(),
            path: PathBuf::from(format!("/tmp/{source}")),
            manifest: PluginManifest {
                name: name.to_string(),
                description: None,
                version: None,
                agents: None,
                user_config: None,
                hooks: None,
                mcp_servers: None,
                lsp_servers: None,
                channels: None,
                ..Default::default()
            },
            agents_path: None,
            agents_paths: Vec::new(),
            enabled,
            is_builtin: false,
            hooks_config: None,
            mcp_servers: Default::default(),
            lsp_servers: Default::default(),
            ..Default::default()
        }
    }

    fn merge_plugin_sources_drops_managed_session_plugins_and_preserves_precedence() {
        let session_alpha = test_loaded_plugin("alpha", "alpha@inline", true);
        let session_beta = test_loaded_plugin("beta", "beta@inline", true);
        let marketplace_alpha = test_loaded_plugin("alpha", "alpha@market", true);
        let marketplace_beta = test_loaded_plugin("beta", "beta@market", true);
        let builtin_gamma = test_loaded_plugin("gamma", "gamma@builtin", true);
        let managed = HashSet::from(["alpha".to_string()]);

        let (plugins, errors) = merge_plugin_sources(PluginSources {
            session: &[session_alpha, session_beta],
            marketplace: &[marketplace_alpha, marketplace_beta],
            builtin: &[builtin_gamma],
            managed_names: Some(&managed),
        });

        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.source.as_str())
                .collect::<Vec<_>>(),
            vec!["beta@inline", "alpha@market", "gamma@builtin"]
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].source(), "alpha@inline");
        assert!(
            crate::types::plugin::get_plugin_error_message(&errors[0])
                .contains("plugin is locked by managed settings")
        );
    }

    #[tokio::test]
    async fn load_session_only_plugins_uses_inline_source_and_skips_missing_paths() {
        let root = temp_dir("inline");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"inline-plugin"}"#,
        );
        let missing = root.join("missing");
        let (plugins, errors) = load_session_only_plugins(&[root.clone(), missing.clone()]).await;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].source, "inline-plugin@inline");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            match &errors[0] {
                PluginError::PathNotFound {
                    path,
                    component: PluginComponent::Commands,
                    ..
                } => Some(Path::new(path)),
                _ => None,
            },
            Some(missing.as_path())
        );
    }

    #[test]
    fn add_dir_enabled_plugins_merge_lowest_priority_and_local_file_wins_within_dir() {
        let first = temp_dir("add-dir-first");
        let second = temp_dir("add-dir-second");
        write_file(
            &first.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"alpha@market":true,"beta@market":true}}"#,
        );
        write_file(
            &first.join(".claude/settings.local.json"),
            r#"{"enabledPlugins":{"beta@market":false}}"#,
        );
        write_file(
            &second.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"alpha@market":false,"gamma@market":true}}"#,
        );
        crate::bootstrap::state::set_additional_directories_for_claude_md(vec![first, second]);
        let add_dir = super::super::add_dir_plugin_settings::get_add_dir_enabled_plugins();
        assert_eq!(add_dir.get("alpha@market"), Some(&Value::Bool(false)));
        assert_eq!(add_dir.get("beta@market"), Some(&Value::Bool(false)));
        assert_eq!(add_dir.get("gamma@market"), Some(&Value::Bool(true)));

        let settings = crate::utils::settings::types::SettingsJson {
            enabled_plugins: Some(serde_json::json!({
                "alpha@market": true,
                "delta@market": true,
            })),
            ..Default::default()
        };
        let merged = merged_enabled_plugins_with_add_dir(&settings).unwrap();
        assert_eq!(merged.get("alpha@market"), Some(&Value::Bool(true)));
        assert_eq!(merged.get("delta@market"), Some(&Value::Bool(true)));
    }

    #[test]
    fn plugin_manifest_hooks_and_mcp_servers_are_carried_without_execution() {
        let root = temp_dir("manifest-mcp-hooks");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
              "name":"toolbox",
              "hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"echo stop"}]}]},
              "mcpServers":{"docs":{"type":"stdio","command":"docs-mcp"}}
            }"#,
        );

        let (plugin, errors) =
            create_plugin_from_path_for_test(&root, "toolbox@inline", true, "toolbox");
        assert!(errors.is_empty(), "errors={errors:?}");
        assert_eq!(
            plugin.hooks_config.as_ref().and_then(|v| v.get("Stop")),
            plugin.manifest.hooks.as_ref().and_then(|v| v.get("Stop"))
        );
        // createPluginFromPath leaves mcpServers on manifest; integration loads
        // and expands it later. The old agent-only constructor copied it early.
        assert!(plugin.mcp_servers.is_none());
        assert_eq!(
            plugin
                .manifest
                .mcp_servers
                .as_ref()
                .and_then(|v| v["docs"]["command"].as_str()),
            Some("docs-mcp")
        );
    }
    #[test]
    fn plugin_settings_schema_reuses_canonical_agent_validation_and_strips_other_keys() {
        assert_eq!(
            parse_plugin_settings(
                &serde_json::json!({"agent":"worker","permissions":{"defaultMode":"bypassPermissions"}})
            ),
            Some(serde_json::json!({"agent":"worker"}))
        );
        for raw in [
            serde_json::json!({"agent":42}),
            serde_json::json!({"other":"worker"}),
            serde_json::json!([]),
            serde_json::Value::Null,
        ] {
            assert_eq!(parse_plugin_settings(&raw), None);
        }
    }

    #[tokio::test]
    async fn full_constructor_combines_components_hooks_and_file_settings_without_early_mcp_loading()
     {
        let root = temp_dir("full-constructor");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"complete","commands":{"inline":{"content":"hello"},"file":{"source":"./extra/command.md"}},"agents":"./extra/agent.md","skills":["./extra/skill"],"outputStyles":"./extra/style.md","hooks":["./additional.json",{"Stop":[{"hooks":[{"type":"command","command":"third"}]}]}],"mcpServers":{"docs":{"command":"docs-mcp"}},"settings":{"agent":"manifest-agent"}}"#,
        );
        write_file(&root.join("extra/command.md"), "command");
        write_file(&root.join("extra/agent.md"), "agent");
        write_file(&root.join("extra/skill/SKILL.md"), "skill");
        write_file(&root.join("extra/style.md"), "style");
        write_file(
            &root.join("hooks/hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"first"}]}]}}"#,
        );
        write_file(
            &root.join("additional.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"second"}]}]}}"#,
        );
        write_file(
            &root.join("settings.json"),
            r#"{"agent":"file-agent","permissions":{"defaultMode":"bypassPermissions"}}"#,
        );
        let (plugin, errors) =
            create_plugin_from_path(&root, "complete@market", true, "fallback", true)
                .await
                .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(plugin.commands_paths, vec![root.join("extra/command.md")]);
        assert!(
            plugin
                .commands_metadata
                .as_ref()
                .unwrap()
                .get("inline")
                .is_some()
        );
        assert_eq!(plugin.agents_paths, vec![root.join("extra/agent.md")]);
        assert_eq!(plugin.skills_paths, vec![root.join("extra/skill")]);
        assert_eq!(
            plugin.output_styles_paths,
            vec![root.join("extra/style.md")]
        );
        assert_eq!(
            plugin.hooks_config.as_ref().unwrap()["Stop"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["hooks"][0]["command"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
        assert_eq!(
            plugin.settings,
            Some(serde_json::json!({"agent":"file-agent"}))
        );
        assert!(plugin.mcp_servers.is_none());
        assert!(plugin.manifest.mcp_servers.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copy_dir_preserves_internal_external_and_broken_symlink_source_contract() {
        let root = temp_dir("copy-links");
        let src = root.join("source");
        let dst = root.join("destination");
        write_file(&src.join("inside.txt"), "inside");
        write_file(&root.join("external.txt"), "external");
        std::os::unix::fs::symlink("inside.txt", src.join("internal-link")).unwrap();
        std::os::unix::fs::symlink("../external.txt", src.join("external-link")).unwrap();
        std::os::unix::fs::symlink("missing", src.join("broken-link")).unwrap();
        copy_dir(&src, &dst).await.unwrap();
        assert_eq!(
            std::fs::read_link(dst.join("internal-link")).unwrap(),
            PathBuf::from("inside.txt")
        );
        assert_eq!(
            std::fs::read_link(dst.join("external-link")).unwrap(),
            std::fs::canonicalize(root.join("external.txt")).unwrap()
        );
        assert_eq!(
            std::fs::read_link(dst.join("broken-link")).unwrap(),
            PathBuf::from("missing")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn node_join_normalizes_base_and_preserves_relative_parent_segments() {
        assert_eq!(
            node_path_join(Path::new("/a/../b"), "./c/../d"),
            PathBuf::from("/b/d")
        );
        assert_eq!(node_path_join(Path::new("/a"), "/b"), PathBuf::from("/a/b"));
        assert_eq!(
            node_path_join(Path::new("../a"), "../../b"),
            PathBuf::from("../../b")
        );
        assert_eq!(
            node_path_join(Path::new("/"), "../../b"),
            PathBuf::from("/b")
        );
    }
    #[test]
    fn temporary_cache_names_preserve_official_prefix_kind_timestamp_and_radix36_nonce() {
        for (source, kind) in [
            (Value::String("./local".into()), "local"),
            (serde_json::json!({"source":"github"}), "github"),
            (serde_json::json!({"source":"url"}), "git"),
            (serde_json::json!({"source":"git-subdir"}), "subdir"),
        ] {
            let name = generate_temporary_cache_name_for_plugin(&source);
            let parts = name.split('_').collect::<Vec<_>>();
            assert_eq!(parts.len(), 4);
            assert_eq!(parts[0], "temp");
            assert_eq!(parts[1], kind);
            assert!(parts[2].parse::<u64>().unwrap() > 0);
            assert_eq!(parts[3].len(), 6);
            assert!(
                parts[3]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
            );
        }
    }
    #[test]
    fn manifest_agent_paths_follow_node_join_normalization() {
        let root = PathBuf::from("/tmp/plugin-root");
        assert_eq!(
            node_path_join(&root, "/agents/reviewer.md"),
            PathBuf::from("/tmp/plugin-root/agents/reviewer.md")
        );
        assert_eq!(
            node_path_join(&root, "../shared/reviewer.md"),
            PathBuf::from("/tmp/shared/reviewer.md")
        );
    }

    #[tokio::test]
    async fn session_plugin_paths_resolve_without_symlink_canonicalization() {
        let cwd = std::env::current_dir().expect("cwd");
        let relative = format!("missing-plugin-{}", uuid::Uuid::new_v4());
        let (plugins, errors) = load_session_only_plugins(&[PathBuf::from(&relative)]).await;
        assert!(plugins.is_empty());
        assert!(
            matches!(errors.as_slice(), [PluginError::PathNotFound { path, .. }] if path == &cwd.join(&relative).display().to_string())
        );
    }
}
