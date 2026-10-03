//! Maps to: CC `utils/plugins/marketplaceManager.ts`.
//! Source-owned configuration, materialization, Git/URL caches, memoized catalog
//! lookup, registration, refresh and removal. Native HTTP/ZIP representations
//! retain the explicitly documented transport boundaries below.

use std::path::{Path, PathBuf};

use crate::utils::process_env::JsTruthy;

/// Maps to: CC `utils/plugins/marketplaceManager.ts:2188-2191,2220-2223`
/// anonymous cache-only lookup result. The unchecked first configuration read
/// can return any installLocation value if the second read observes a change;
/// None preserves undefined and Some(Null) preserves explicit null.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketplacePluginMetadata {
    pub entry: serde_json::Value,
    pub marketplace_install_location: Option<serde_json::Value>,
}

/// Maps to: CC `marketplaceManager.ts:102-104#getKnownMarketplacesFile`.
fn get_known_marketplaces_file() -> PathBuf {
    // Node path.join performs lexical normalization without filesystem access
    // or NFC. PathBuf::join alone would send missing/../ segments to the OS.
    let joined = crate::utils::plugins::plugin_directories::get_plugins_directory()
        .join("known_marketplaces.json");
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if normalized.file_name().is_some_and(|name| name != "..") {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Maps to: CC `marketplaceManager.ts:129#KnownMarketplacesConfig`.
/// Schema-validated JSON object; preserve own-property names and their order.
pub type KnownMarketplacesConfig = serde_json::Map<String, serde_json::Value>;

/// Maps to: CC `marketplaceManager.ts:264-299#loadKnownMarketplacesConfig`.
/// The original throwing reader, with activeFs async I/O and Result carriers.
/// Partial shared value domain: JSON parser diagnostics retain native wording;
/// `.to_json()` projects nonfinite numbers to null and lone UTF-16 to replacement
/// characters, so diagnostics/default_config for those values still differ.
pub fn load_known_marketplaces_config()
-> impl std::future::Future<Output = anyhow::Result<KnownMarketplacesConfig>> {
    use crate::utils::debug::{DebugLogLevel, log_for_debugging_with_level};
    use crate::utils::errors::ConfigParseError;
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let config_file = get_known_marketplaces_file();
    let pending = fs.read_file(
        &config_file,
        crate::utils::fs_operations::BufferEncoding::Utf8,
    );
    async move {
        let loaded: anyhow::Result<KnownMarketplacesConfig> = async {
            let bytes = pending
                .await
                .map(|text| text.to_string_lossy().into_bytes())
                .map_err(anyhow::Error::new)?;
            let content = String::from_utf8_lossy(&bytes);
            let data = crate::utils::slow_operations::json_parse(&content)?.to_json();
            let parsed = crate::utils::zod::safe_parse(
                super::schemas::known_marketplaces_file_schema(),
                &data,
            )
            .map_err(|error| {
                let issues = error
                    .issues
                    .iter()
                    .map(|issue| {
                        let path = issue
                            .path
                            .iter()
                            .map(|part| match part {
                                crate::utils::zod::PathSegment::Key(key) => key.clone(),
                                crate::utils::zod::PathSegment::Index(index) => index.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(".");
                        format!("{path}: {}", issue.message)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let message = format!("Marketplace configuration file is corrupted: {issues}");
                log_for_debugging_with_level(&message, DebugLogLevel::Error);
                ConfigParseError::new(message, config_file.to_string_lossy(), data)
            })?;
            Ok(parsed
                .as_object()
                .expect("KnownMarketplacesFileSchema returns a record")
                .clone())
        }
        .await;
        match loaded {
            Ok(config) => Ok(config),
            Err(error) if crate::utils::errors::is_enoent(&error) => {
                Ok(KnownMarketplacesConfig::new())
            }
            Err(error) if error.is::<ConfigParseError>() => Err(error),
            Err(error) => {
                let message = format!("Failed to load marketplace configuration: {error}");
                log_for_debugging_with_level(&message, DebugLogLevel::Error);
                Err(anyhow::anyhow!(message))
            }
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:309-317#loadKnownMarketplacesConfigSafe`.
pub fn load_known_marketplaces_config_safe()
-> impl std::future::Future<Output = KnownMarketplacesConfig> {
    let pending = load_known_marketplaces_config();
    async move { pending.await.unwrap_or_default() }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1372-1405#parseFileWithSchema`.
/// CC safeParse schema argument ≙ the established Zod v4 runtime carrier.
/// JSON parser wording/nonfinite/UTF-16 projections retain the shared parser
/// boundary documented on load_known_marketplaces_config.
fn parse_file_with_schema(
    file_path: &Path,
    schema: &crate::utils::zod::Schema,
) -> impl std::future::Future<Output = anyhow::Result<serde_json::Value>> {
    use crate::utils::errors::ConfigParseError;
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let pending = fs.read_file(file_path, crate::utils::fs_operations::BufferEncoding::Utf8);
    async move {
        let bytes = pending
            .await
            .map(|text| text.to_string_lossy().into_bytes())
            .map_err(anyhow::Error::new)?;
        let content = String::from_utf8_lossy(&bytes);
        let data = crate::utils::slow_operations::json_parse(&content)
            .map_err(|error| {
                ConfigParseError::new(
                    format!("Invalid JSON in {}: {error}", file_path.display()),
                    file_path.to_string_lossy(),
                    serde_json::Value::String(content.to_string()),
                )
            })?
            .to_json();
        crate::utils::zod::safe_parse(schema, &data).map_err(|error| {
            let issues = error
                .issues
                .iter()
                .map(|issue| {
                    let path = issue
                        .path
                        .iter()
                        .map(|part| match part {
                            crate::utils::zod::PathSegment::Key(key) => key.clone(),
                            crate::utils::zod::PathSegment::Index(index) => index.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(".");
                    format!("{path}: {}", issue.message)
                })
                .collect::<Vec<_>>()
                .join(", ");
            ConfigParseError::new(
                format!("Invalid schema: {} {issues}", file_path.display()),
                file_path.to_string_lossy(),
                data,
            )
            .into()
        })
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:2058-2074#readCachedMarketplace`.
pub(crate) async fn read_cached_marketplace(
    install_location: &Path,
) -> anyhow::Result<serde_json::Value> {
    use crate::utils::errors::{ConfigParseError, get_errno_code};
    // Node join normalizes this nested path lexically, without resolving links.
    // The fallback below uses the original installLocation spelling unchanged.
    let joined = install_location
        .join(".claude-plugin")
        .join("marketplace.json");
    let mut nested_path = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if nested_path.file_name().is_some_and(|name| name != "..") {
                    nested_path.pop();
                } else if !nested_path.has_root() {
                    nested_path.push("..");
                }
            }
            component => nested_path.push(component.as_os_str()),
        }
    }
    match parse_file_with_schema(&nested_path, super::schemas::plugin_marketplace_schema()).await {
        Ok(marketplace) => return Ok(marketplace),
        Err(error) if error.is::<ConfigParseError>() => return Err(error),
        Err(error) if !matches!(get_errno_code(&error), Some("ENOENT" | "ENOTDIR")) => {
            return Err(error);
        }
        Err(_) => {}
    }
    parse_file_with_schema(
        install_location,
        super::schemas::plugin_marketplace_schema(),
    )
    .await
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:2081-2107#getMarketplaceCacheOnly`.
/// Configuration is intentionally unchecked here; only the catalog is validated.
pub fn get_marketplace_cache_only(
    name: &str,
) -> impl std::future::Future<Output = Option<serde_json::Value>> {
    use crate::utils::debug::{DebugLogLevel, log_for_debugging_with_level};
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let config_file = get_known_marketplaces_file();
    let pending = fs.read_file(
        &config_file,
        crate::utils::fs_operations::BufferEncoding::Utf8,
    );
    async move {
        let loaded: anyhow::Result<Option<serde_json::Value>> = async {
            let bytes = pending
                .await
                .map(|text| text.to_string_lossy().into_bytes())
                .map_err(anyhow::Error::new)?;
            #[cfg(test)]
            let bytes = tests::CONFIG_READS
                .try_with(|reads| reads.borrow_mut().pop_front())
                .ok()
                .flatten()
                .unwrap_or(bytes);
            let config =
                crate::utils::slow_operations::json_parse(&String::from_utf8_lossy(&bytes))?;
            if config.is_null() {
                anyhow::bail!("null is not an object (evaluating 'config[name]')");
            }
            let Some(entry) = config.get_property(name) else {
                return Ok(None);
            };
            // The JSON carrier retains JS numeric values before projection. These
            // are the source's !entry check, not a KnownMarketplaceSchema gate.
            if matches!(entry.kind, 7 | 9)
                || (entry.kind == 10 && entry.string_units.is_empty())
                || (entry.kind == 11 && entry.number.is_some_and(|n| n == 0.0 || n.is_nan()))
            {
                return Ok(None);
            }
            let location = entry.get_property("installLocation");
            let install_location =
                location
                    .and_then(|location| location.as_str())
                    .ok_or_else(|| {
                        // Node path.join's type check happens before filesystem access.
                        let kind = match location.map(|location| location.kind) {
                            None => "undefined",
                            Some(3) => "array",
                            Some(8 | 9) => "boolean",
                            Some(11) => "number",
                            _ => "object",
                        };
                        anyhow::anyhow!(
                            "The \"paths[0]\" property must be of type string, got {kind}"
                        )
                    })?;
            Ok(Some(
                read_cached_marketplace(Path::new(install_location)).await?,
            ))
        }
        .await;
        match loaded {
            Ok(marketplace) => marketplace,
            Err(error) if crate::utils::errors::is_enoent(&error) => None,
            Err(error) => {
                log_for_debugging_with_level(
                    &format!("Failed to read cached marketplace {name}: {error}"),
                    DebugLogLevel::Warn,
                );
                None
            }
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:2188-2227#getPluginByIdCacheOnly`.
/// Keep both configuration reads: the returned installLocation belongs to the
/// first read, while getMarketplaceCacheOnly resolves the catalog on the second.
pub fn get_plugin_by_id_cache_only(
    plugin_id: &str,
) -> impl std::future::Future<Output = Option<MarketplacePluginMetadata>> {
    let pending = (|| {
        let parsed = super::plugin_identifier::parse_plugin_identifier(plugin_id);
        let marketplace_name = parsed.marketplace.clone()?;
        if parsed.name.is_empty() || marketplace_name.is_empty() {
            return None;
        }
        let fs = crate::utils::fs_operations::get_fs_implementation();
        let config_file = get_known_marketplaces_file();
        let read = fs.read_file(
            &config_file,
            crate::utils::fs_operations::BufferEncoding::Utf8,
        );
        Some((parsed, marketplace_name, read))
    })();
    async move {
        let (parsed, marketplace_name, read) = pending?;
        let bytes = read.await.ok()?.to_string_lossy().into_bytes();
        #[cfg(test)]
        let bytes = tests::CONFIG_READS
            .try_with(|reads| reads.borrow_mut().pop_front())
            .ok()
            .flatten()
            .unwrap_or(bytes);
        let config =
            crate::utils::slow_operations::json_parse(&String::from_utf8_lossy(&bytes)).ok()?;
        let marketplace_config = config.get_property(&marketplace_name)?;
        if matches!(marketplace_config.kind, 7 | 9)
            || (marketplace_config.kind == 10 && marketplace_config.string_units.is_empty())
            || (marketplace_config.kind == 11
                && marketplace_config
                    .number
                    .is_some_and(|n| n == 0.0 || n.is_nan()))
        {
            return None;
        }
        let marketplace = get_marketplace_cache_only(&marketplace_name).await?;
        let plugin = marketplace
            .get("plugins")?
            .as_array()?
            .iter()
            .find(|plugin| {
                plugin.get("name").and_then(serde_json::Value::as_str) == Some(parsed.name.as_str())
            })?;
        Some(MarketplacePluginMetadata {
            entry: plugin.clone(),
            marketplace_install_location: marketplace_config
                .get_property("installLocation")
                .map(crate::utils::json::JsoncValue::to_json),
        })
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:510-513#GIT_NO_PROMPT_ENV`.
const GIT_NO_PROMPT_ENV: [(&str, &str); 2] = [("GIT_TERMINAL_PROMPT", "0"), ("GIT_ASKPASS", "")];
/// Maps to: CC `utils/plugins/marketplaceManager.ts:515-515#DEFAULT_PLUGIN_GIT_TIMEOUT_MS`.
const DEFAULT_PLUGIN_GIT_TIMEOUT_MS: f64 = 120_000.0;

/// Maps to: CC `utils/plugins/marketplaceManager.ts:517-526#getPluginGitTimeoutMs`.
fn get_plugin_git_timeout_ms() -> f64 {
    if let Some(value) =
        crate::utils::process_env::var("CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS").truthy()
    {
        // ECMAScript trim and decimal-prefix parseInt; preserve Number rounding.
        let trimmed = value.trim_start_matches(|c: char| matches!(c, '\u{9}'..='\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
        let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
        let count = digits.bytes().take_while(u8::is_ascii_digit).count();
        if count > 0 {
            let parsed = digits[..count].parse::<f64>().unwrap_or(f64::NAN)
                * if trimmed.starts_with('-') { -1.0 } else { 1.0 };
            if !parsed.is_nan() && parsed > 0.0 {
                return parsed;
            }
        }
    }
    DEFAULT_PLUGIN_GIT_TIMEOUT_MS
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:531` options object.
#[derive(Default)]
pub(crate) struct GitPullOptions<'a> {
    pub disable_credential_helper: bool,
    pub sparse_paths: Option<&'a [String]>,
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:528-582#gitPull`.
/// Native async options use the canonical A6/A7 eager process runner. Duration
/// represents finite native timer values only; out-of-range JS timers remain a
/// representation seam (Duration::MAX), with no new marketplace timeout policy.
pub(crate) async fn git_pull(
    cwd: &Path,
    git_ref: Option<&str>,
    options: Option<GitPullOptions<'_>>,
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    use crate::utils::debug::log_for_debugging;
    use crate::utils::exec_file_no_throw::{
        ExecFileStdin, ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
    };
    log_for_debugging(&format!(
        "git pull: cwd={} ref={}",
        cwd.display(),
        git_ref.unwrap_or("default")
    ));
    let env = crate::utils::process_env::snapshot()
        .iter()
        .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
        .chain(GIT_NO_PROMPT_ENV.map(|(k, v)| (k.into(), Some(v.into()))))
        .collect::<Vec<_>>();
    let options = options.unwrap_or_default();
    let credential_args = if options.disable_credential_helper {
        vec!["-c", "credential.helper="]
    } else {
        vec![]
    };
    if let Some(git_ref) = git_ref.filter(|s| !s.is_empty()) {
        let fetch_result = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &[credential_args.as_slice(), &["fetch", "origin", git_ref]].concat(),
            ExecFileWithCwdOptions {
                cwd: Some(cwd),
                timeout: std::time::Duration::try_from_secs_f64(
                    get_plugin_git_timeout_ms() / 1000.0,
                )
                .unwrap_or(std::time::Duration::MAX),
                stdin: ExecFileStdin::Ignore,
                env: &env,
                ..Default::default()
            },
        )
        .await;
        if fetch_result.code != 0 {
            return enhance_git_pull_error_messages(fetch_result);
        }
        let checkout_result = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &[credential_args.as_slice(), &["checkout", git_ref]].concat(),
            ExecFileWithCwdOptions {
                cwd: Some(cwd),
                timeout: std::time::Duration::try_from_secs_f64(
                    get_plugin_git_timeout_ms() / 1000.0,
                )
                .unwrap_or(std::time::Duration::MAX),
                stdin: ExecFileStdin::Ignore,
                env: &env,
                ..Default::default()
            },
        )
        .await;
        if checkout_result.code != 0 {
            return enhance_git_pull_error_messages(checkout_result);
        }
        let pull_result = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &[credential_args.as_slice(), &["pull", "origin", git_ref]].concat(),
            ExecFileWithCwdOptions {
                cwd: Some(cwd),
                timeout: std::time::Duration::try_from_secs_f64(
                    get_plugin_git_timeout_ms() / 1000.0,
                )
                .unwrap_or(std::time::Duration::MAX),
                stdin: ExecFileStdin::Ignore,
                env: &env,
                ..Default::default()
            },
        )
        .await;
        if pull_result.code != 0 {
            return enhance_git_pull_error_messages(pull_result);
        }
        git_submodule_update(cwd, &credential_args, &env, options.sparse_paths).await;
        return pull_result;
    }
    let result = exec_file_no_throw_with_cwd_options(
        &crate::utils::git::git_exe().to_string_lossy(),
        &[credential_args.as_slice(), &["pull", "origin", "HEAD"]].concat(),
        ExecFileWithCwdOptions {
            cwd: Some(cwd),
            timeout: std::time::Duration::try_from_secs_f64(get_plugin_git_timeout_ms() / 1000.0)
                .unwrap_or(std::time::Duration::MAX),
            stdin: ExecFileStdin::Ignore,
            env: &env,
            ..Default::default()
        },
    )
    .await;
    if result.code != 0 {
        return enhance_git_pull_error_messages(result);
    }
    git_submodule_update(cwd, &credential_args, &env, options.sparse_paths).await;
    result
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:609-644#gitSubmoduleUpdate`.
fn git_submodule_update(
    cwd: &Path,
    credential_args: &[&str],
    env: &[(std::ffi::OsString, Option<std::ffi::OsString>)],
    sparse_paths: Option<&[String]>,
) -> impl std::future::Future<Output = ()> {
    use crate::utils::exec_file_no_throw::{
        ExecFileStdin, ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
    };
    let pending = if sparse_paths.is_some_and(|paths| !paths.is_empty()) {
        None
    } else {
        let gitmodules =
            crate::utils::fs_operations::native::join_path(cwd, Path::new(".gitmodules"));
        Some(crate::utils::fs_operations::get_fs_implementation().stat(&gitmodules))
    };
    async move {
        let Some(pending) = pending else {
            return;
        };
        if pending.await.is_err() {
            return;
        }
        let result = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &[
                &[
                    "-c",
                    "core.sshCommand=ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
                ][..],
                credential_args,
                &[
                    "submodule",
                    "update",
                    "--init",
                    "--recursive",
                    "--depth",
                    "1",
                ],
            ]
            .concat(),
            ExecFileWithCwdOptions {
                cwd: Some(cwd),
                timeout: std::time::Duration::try_from_secs_f64(
                    get_plugin_git_timeout_ms() / 1000.0,
                )
                .unwrap_or(std::time::Duration::MAX),
                stdin: ExecFileStdin::Ignore,
                env,
                ..Default::default()
            },
        )
        .await;
        if result.code != 0 {
            crate::utils::debug::log_for_debugging_with_level(
                &format!("git submodule update failed (non-fatal): {}", result.stderr),
                crate::utils::debug::DebugLogLevel::Warn,
            );
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:649-709#enhanceGitPullErrorMessages`.
fn enhance_git_pull_error_messages(
    mut result: crate::utils::exec_file_no_throw::ExecFileOutput,
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    if result.code == 0 {
        return result;
    }
    let message = if result
        .error
        .as_deref()
        .is_some_and(|e| e.contains("timed out"))
    {
        let timeout_sec = ryu_js::Buffer::new()
            .format((get_plugin_git_timeout_ms() / 1000.0).round())
            .to_owned();
        Some(format!(
            "Git pull timed out after {timeout_sec}s. Try increasing the timeout via CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS environment variable."
        ))
    } else if result
        .stderr
        .contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
    {
        Some("SSH host key for this marketplace's git host has changed (server key rotation or possible MITM). Remove the stale entry with: ssh-keygen -R <host>\nThen connect once manually to accept the new key.".into())
    } else if result.stderr.contains("Host key verification failed") {
        Some("SSH host key verification failed while updating marketplace. The host key is not in your known_hosts file. Connect once manually to add it (e.g., ssh -T git@<host>), or remove and re-add the marketplace with an HTTPS URL.".into())
    } else if result.stderr.contains("Permission denied (publickey)")
        || result
            .stderr
            .contains("Could not read from remote repository")
    {
        Some("SSH authentication failed while updating marketplace. Please ensure your SSH keys are configured.".into())
    } else if result.stderr.contains("timed out")
        || result.stderr.contains("Could not resolve host")
    {
        Some(
            "Network error while updating marketplace. Please check your internet connection."
                .into(),
        )
    } else {
        None
    };
    if let Some(message) = message {
        result.stderr = format!("{message}\n\nOriginal error: {}", result.stderr);
    }
    result
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:723-761#isGitHubSshLikelyConfigured`.
async fn is_github_ssh_likely_configured() -> bool {
    use crate::utils::exec_file_no_throw::{
        ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
    };
    // The native runner resolves ordinary process failures. catch_unwind is
    // the bounded native exception representation for the source outer catch.
    use futures::FutureExt;
    let checked = std::panic::AssertUnwindSafe(async {
        let result = exec_file_no_throw_with_cwd_options(
            "ssh",
            &[
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "StrictHostKeyChecking=yes",
                "git@github.com",
            ],
            ExecFileWithCwdOptions {
                timeout: std::time::Duration::from_millis(3000),
                ..Default::default()
            },
        )
        .await;
        let configured = result.code == 1
            && (result.stderr.contains("successfully authenticated")
                || result.stdout.contains("successfully authenticated"));
        crate::utils::debug::log_for_debugging(&format!(
            "SSH config check: code={} configured={configured}",
            result.code
        ));
        configured
    })
    .catch_unwind()
    .await;
    match checked {
        Ok(configured) => configured,
        Err(error) => {
            let message = error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "non-string native panic".into());
            crate::utils::debug::log_for_debugging_with_level(
                &format!("SSH configuration check failed: {message}"),
                crate::utils::debug::DebugLogLevel::Warn,
            );
            false
        }
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:767-775#isAuthenticationError`.
fn is_authentication_error(stderr: &str) -> bool {
    stderr.contains("Authentication failed")
        || stderr.contains("could not read Username")
        || stderr.contains("terminal prompts disabled")
        || stderr.contains("403")
        || stderr.contains("401")
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:781-784#extractSshHost`.
fn extract_ssh_host(git_url: &str) -> Option<&str> {
    let regex = regress::Regex::new(r"^[^@]+@([^:]+):").expect("source regex is valid");
    regex
        .find(git_url)
        .and_then(|m| m.group(1))
        .map(|r| &git_url[r])
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:803-985#gitClone`.
/// ExecFileOutput retains the source's returned/spread stdout/error values;
/// fresh two-field result objects use empty stdout/None error in the native
/// carrier. Path strings share its valid-Unicode boundary; native timer range
/// is the explicitly partial Duration boundary documented on git_pull.
pub(crate) async fn git_clone(
    git_url: &str,
    target_path: &Path,
    git_ref: Option<&str>,
    sparse_paths: Option<&[String]>,
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
    use crate::utils::exec_file_no_throw::{
        ExecFileOutput, ExecFileStdin, ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
    };
    let use_sparse = sparse_paths.is_some_and(|s| !s.is_empty());
    let mut args = vec![
        "-c",
        "core.sshCommand=ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
        "clone",
        "--depth",
        "1",
    ];
    if use_sparse {
        args.extend(["--filter=blob:none", "--no-checkout"]);
    } else {
        args.extend(["--recurse-submodules", "--shallow-submodules"]);
    }
    if let Some(git_ref) = git_ref.filter(|s| !s.is_empty()) {
        args.extend(["--branch", git_ref]);
    }
    let target = target_path.to_string_lossy();
    args.extend([git_url, &target]);
    let timeout_ms = get_plugin_git_timeout_ms();
    let timeout = std::time::Duration::try_from_secs_f64(timeout_ms / 1000.0)
        .unwrap_or(std::time::Duration::MAX);
    log_for_debugging(&format!(
        "git clone: url={} ref={} timeout={}ms",
        redact_url_credentials(git_url),
        git_ref.unwrap_or("default"),
        ryu_js::Buffer::new().format(timeout_ms)
    ));
    let env = crate::utils::process_env::snapshot()
        .iter()
        .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
        .chain(GIT_NO_PROMPT_ENV.map(|(k, v)| (k.into(), Some(v.into()))))
        .collect::<Vec<_>>();
    let mut result = exec_file_no_throw_with_cwd_options(
        &crate::utils::git::git_exe().to_string_lossy(),
        &args,
        ExecFileWithCwdOptions {
            timeout,
            stdin: ExecFileStdin::Ignore,
            env: &env,
            ..Default::default()
        },
    )
    .await;
    let redacted = redact_url_credentials(git_url);
    if git_url != redacted {
        if let Some(error) = &mut result.error {
            *error = error.replace(git_url, &redacted);
        }
        result.stderr = result.stderr.replace(git_url, &redacted);
    }
    if result.code == 0 {
        if use_sparse {
            let env = crate::utils::process_env::snapshot()
                .iter()
                .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
                .chain(GIT_NO_PROMPT_ENV.map(|(k, v)| (k.into(), Some(v.into()))))
                .collect::<Vec<_>>();
            let mut args = vec!["sparse-checkout", "set", "--cone", "--"];
            args.extend(sparse_paths.unwrap().iter().map(String::as_str));
            let sparse_result = exec_file_no_throw_with_cwd_options(
                &crate::utils::git::git_exe().to_string_lossy(),
                &args,
                ExecFileWithCwdOptions {
                    cwd: Some(target_path),
                    timeout,
                    stdin: ExecFileStdin::Ignore,
                    env: &env,
                    ..Default::default()
                },
            )
            .await;
            if sparse_result.code != 0 {
                return ExecFileOutput {
                    code: sparse_result.code,
                    stderr: format!("git sparse-checkout set failed: {}", sparse_result.stderr),
                    stdout: String::new(),
                    error: None,
                };
            }
            let env = crate::utils::process_env::snapshot()
                .iter()
                .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
                .chain(GIT_NO_PROMPT_ENV.map(|(k, v)| (k.into(), Some(v.into()))))
                .collect::<Vec<_>>();
            let checkout_result = exec_file_no_throw_with_cwd_options(
                &crate::utils::git::git_exe().to_string_lossy(),
                &["checkout", "HEAD"],
                ExecFileWithCwdOptions {
                    cwd: Some(target_path),
                    timeout,
                    stdin: ExecFileStdin::Ignore,
                    env: &env,
                    ..Default::default()
                },
            )
            .await;
            if checkout_result.code != 0 {
                return ExecFileOutput {
                    code: checkout_result.code,
                    stderr: format!(
                        "git checkout after sparse-checkout failed: {}",
                        checkout_result.stderr
                    ),
                    stdout: String::new(),
                    error: None,
                };
            }
        }
        log_for_debugging(&format!(
            "git clone succeeded: {}",
            redact_url_credentials(git_url)
        ));
        return result;
    }
    log_for_debugging_with_level(
        &format!(
            "git clone failed: url={} code={} error={} stderr={}",
            redact_url_credentials(git_url),
            result.code,
            result.error.as_deref().unwrap_or("none"),
            result.stderr
        ),
        DebugLogLevel::Warn,
    );
    if result
        .error
        .as_deref()
        .is_some_and(|e| e.contains("timed out"))
    {
        result.stderr = format!(
            "Git clone timed out after {}s. The repository may be too large for the current timeout. Set CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS to increase it (e.g., 300000 for 5 minutes).\n\nOriginal error: {}",
            ryu_js::Buffer::new().format((timeout_ms / 1000.0).round()),
            result.stderr
        );
        return result;
    }
    if !result.stderr.is_empty() {
        let message = if result
            .stderr
            .contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        {
            let host = extract_ssh_host(git_url).unwrap_or("<host>");
            Some(format!(
                "SSH host key has changed (server key rotation or possible MITM). Remove the stale known_hosts entry:\n  ssh-keygen -R {host}\nThen connect once manually to verify and accept the new key."
            ))
        } else if result.stderr.contains("Host key verification failed") {
            let host = extract_ssh_host(git_url).unwrap_or("<host>");
            Some(format!(
                "SSH host key is not in your known_hosts file. To add it, connect once manually (this will show the fingerprint for you to verify):\n  ssh -T git@{host}\n\nOr use an HTTPS URL instead (recommended for public repos)."
            ))
        } else if result.stderr.contains("Permission denied (publickey)")
            || result
                .stderr
                .contains("Could not read from remote repository")
        {
            Some("SSH authentication failed. Please ensure your SSH keys are configured for GitHub, or use an HTTPS URL instead.".into())
        } else if is_authentication_error(&result.stderr) {
            Some("HTTPS authentication failed. Please ensure your credential helper is configured (e.g., gh auth login).".into())
        } else if result.stderr.contains("timed out")
            || result.stderr.contains("timeout")
            || result.stderr.contains("Could not resolve host")
        {
            Some("Network error or timeout while cloning repository. Please check your internet connection and try again.".into())
        } else {
            None
        };
        if let Some(message) = message {
            result.stderr = format!("{message}\n\nOriginal error: {}", result.stderr);
            return result;
        }
    }
    if result.stderr.is_empty() {
        return ExecFileOutput { code: result.code, stderr: result.error.filter(|e| !e.is_empty()).unwrap_or_else(|| format!("git clone exited with code {} (no stderr output). Run with --debug to see the full command.",result.code)), stdout:String::new(), error:None };
    }
    result
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:998-998#MarketplaceProgressCallback`.
/// Result carries a source callback exception; native unwinding is caught too.
pub type MarketplaceProgressCallback<'a> = dyn Fn(&str) -> anyhow::Result<()> + Send + Sync + 'a;

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1007-1019#safeCallProgress`.
fn safe_call_progress(on_progress: Option<&MarketplaceProgressCallback<'_>>, message: &str) {
    let Some(on_progress) = on_progress else {
        return;
    };
    let error =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_progress(message))) {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(error) => error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "non-string native panic".into()),
        };
    crate::utils::debug::log_for_debugging_with_level(
        &format!("Progress callback error: {error}"),
        crate::utils::debug::DebugLogLevel::Warn,
    );
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1034-1061#reconcileSparseCheckout`.
pub(crate) async fn reconcile_sparse_checkout(
    cwd: &Path,
    sparse_paths: Option<&[String]>,
) -> crate::utils::exec_file_no_throw::ExecFileOutput {
    use crate::utils::exec_file_no_throw::{
        ExecFileOutput, ExecFileStdin, ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
    };
    let env = crate::utils::process_env::snapshot()
        .iter()
        .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
        .chain(GIT_NO_PROMPT_ENV.map(|(k, v)| (k.into(), Some(v.into()))))
        .collect::<Vec<_>>();
    if let Some(sparse_paths) = sparse_paths.filter(|s| !s.is_empty()) {
        let mut args = vec!["sparse-checkout", "set", "--cone", "--"];
        args.extend(sparse_paths.iter().map(String::as_str));
        return exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &args,
            ExecFileWithCwdOptions {
                cwd: Some(cwd),
                timeout: std::time::Duration::try_from_secs_f64(
                    get_plugin_git_timeout_ms() / 1000.0,
                )
                .unwrap_or(std::time::Duration::MAX),
                stdin: ExecFileStdin::Ignore,
                env: &env,
                ..Default::default()
            },
        )
        .await;
    }
    let check = exec_file_no_throw_with_cwd_options(
        &crate::utils::git::git_exe().to_string_lossy(),
        &["config", "--get", "core.sparseCheckout"],
        ExecFileWithCwdOptions {
            cwd: Some(cwd),
            stdin: ExecFileStdin::Ignore,
            env: &env,
            ..Default::default()
        },
    )
    .await;
    let trimmed = check.stdout.trim_matches(|c: char| matches!(c, '\u{9}'..='\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
    if check.code == 0 && trimmed == "true" {
        return ExecFileOutput { code:1, stderr:"sparsePaths removed from config but repository is sparse; re-cloning for full checkout".into(), stdout:String::new(), error:None };
    }
    ExecFileOutput {
        code: 0,
        stderr: String::new(),
        stdout: String::new(),
        error: None,
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1084-1178#cacheMarketplaceFromGit`.
/// Option bool is the source options.disableCredentialHelper projection; no
/// materialization/validation is added beyond this source function's progress.
pub(crate) async fn cache_marketplace_from_git(
    git_url: &str,
    cache_path: &Path,
    git_ref: Option<&str>,
    sparse_paths: Option<&[String]>,
    on_progress: Option<&MarketplaceProgressCallback<'_>>,
    disable_credential_helper: bool,
) -> anyhow::Result<()> {
    use super::fetch_telemetry::{
        PluginFetchOutcome, PluginFetchSource, classify_fetch_error, log_plugin_fetch,
    };
    use crate::utils::debug::{DebugLogLevel, log_for_debugging, log_for_debugging_with_level};
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let timeout_sec = ryu_js::Buffer::new()
        .format((get_plugin_git_timeout_ms() / 1000.0).round())
        .to_owned();
    safe_call_progress(
        on_progress,
        &format!("Refreshing marketplace cache (timeout: {timeout_sec}s)…"),
    );
    let reconcile_result = reconcile_sparse_checkout(cache_path, sparse_paths).await;
    if reconcile_result.code == 0 {
        let pull_started = std::time::Instant::now();
        let pull_result = git_pull(
            cache_path,
            git_ref,
            Some(GitPullOptions {
                disable_credential_helper,
                sparse_paths,
            }),
        )
        .await;
        log_plugin_fetch(
            PluginFetchSource::MarketplacePull,
            Some(git_url),
            if pull_result.code == 0 {
                PluginFetchOutcome::Success
            } else {
                PluginFetchOutcome::Failure
            },
            pull_started.elapsed().as_secs_f64() * 1000.0,
            (pull_result.code != 0).then(|| classify_fetch_error(&pull_result.stderr)),
        );
        if pull_result.code == 0 {
            return Ok(());
        }
        log_for_debugging_with_level(
            &format!("git pull failed, will re-clone: {}", pull_result.stderr),
            DebugLogLevel::Warn,
        );
    } else {
        log_for_debugging(&format!(
            "sparse-checkout reconcile requires re-clone: {}",
            reconcile_result.stderr
        ));
    }
    match fs
        .rm(
            cache_path,
            crate::utils::fs_operations::RmOptions {
                recursive: true,
                force: false,
            },
        )
        .await
    {
        Ok(()) => {
            log_for_debugging_with_level(
                &format!(
                    "Found stale marketplace directory at {}, cleaning up to allow re-clone",
                    cache_path.display()
                ),
                DebugLogLevel::Warn,
            );
            safe_call_progress(
                on_progress,
                "Found stale directory, cleaning up and re-cloning…",
            );
        }
        Err(error) if crate::utils::errors::io_errno_code(&error) == Some("ENOENT") => {}
        Err(error) => {
            let message = error.to_string();
            anyhow::bail!(
                "Failed to clean up existing marketplace directory. Please manually delete the directory at {} and try again.\n\nTechnical details: {message}",
                cache_path.display()
            );
        }
    }
    let ref_message = git_ref
        .filter(|s| !s.is_empty())
        .map(|s| format!(" (ref: {s})"))
        .unwrap_or_default();
    safe_call_progress(
        on_progress,
        &format!(
            "Cloning repository (timeout: {timeout_sec}s): {}{ref_message}",
            redact_url_credentials(git_url)
        ),
    );
    let clone_started = std::time::Instant::now();
    let result = git_clone(git_url, cache_path, git_ref, sparse_paths).await;
    log_plugin_fetch(
        PluginFetchSource::MarketplaceClone,
        Some(git_url),
        if result.code == 0 {
            PluginFetchOutcome::Success
        } else {
            PluginFetchOutcome::Failure
        },
        clone_started.elapsed().as_secs_f64() * 1000.0,
        (result.code != 0).then(|| classify_fetch_error(&result.stderr)),
    );
    if result.code != 0 {
        let _ = fs
            .rm(
                cache_path,
                crate::utils::fs_operations::RmOptions {
                    recursive: true,
                    force: true,
                },
            )
            .await;
        anyhow::bail!("Failed to clone marketplace repository: {}", result.stderr);
    }
    safe_call_progress(on_progress, "Clone complete, validating marketplace…");
    Ok(())
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1213-1226#redactUrlCredentials`.
/// WHATWG URL uses the existing url crate carrier (valid Unicode URL subset).
fn redact_url_credentials(url_string: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(url_string) {
        if matches!(parsed.scheme(), "http" | "https")
            && (!parsed.username().is_empty() || parsed.password().is_some_and(|s| !s.is_empty()))
        {
            if !parsed.username().is_empty() {
                let _ = parsed.set_username("***");
            }
            if parsed.password().is_some_and(|s| !s.is_empty()) {
                let _ = parsed.set_password(Some("***"));
            }
            return parsed.to_string();
        }
    }
    url_string.into()
}

// Native representation of Node path.join/resolve (not marketplace policy).
// Normalize the whole string lexically; PathBuf::join incorrectly discards
// the base for later absolute segments. No symlink, tilde or NFC resolution.
macro_rules! marketplace_path {
    ($($part:expr),+ $(,)?) => {{
        let mut joined = String::new();
        $(let part = $part; let part = AsRef::<Path>::as_ref(&part).to_string_lossy(); if !part.is_empty() { if !joined.is_empty() { joined.push(std::path::MAIN_SEPARATOR); } joined.push_str(&part); })+
        let mut normalized = PathBuf::new();
        for component in Path::new(&joined).components() { match component {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { if normalized.file_name().is_some_and(|s|s!="..") {normalized.pop();} else if !normalized.has_root() {normalized.push("..");} },
            c => normalized.push(c.as_os_str()),
        }}
        if normalized.as_os_str().is_empty() {normalized.push(".");}
        normalized
    }};
}
macro_rules! marketplace_resolve {
    ($path:expr) => {{
        let path = AsRef::<Path>::as_ref(&$path).to_path_buf();
        if path.is_absolute() {
            marketplace_path!(path)
        } else {
            marketplace_path!(std::env::current_dir()?, path)
        }
    }};
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:93-96#LoadedPluginMarketplace`.
pub struct LoadedPluginMarketplace {
    pub marketplace: serde_json::Value,
    pub cache_path: PathBuf,
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:110-112#getMarketplacesCacheDir`.
pub fn get_marketplaces_cache_dir() -> PathBuf {
    marketplace_path!(
        super::plugin_directories::get_plugins_directory(),
        "marketplaces"
    )
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:138-152#DeclaredMarketplace`.
/// Validated source object, with the code-only optional sourceIsFallback field.
pub type DeclaredMarketplace = serde_json::Value;
/// Maps to: CC `utils/plugins/marketplaceManager.ts:161-192#getDeclaredMarketplaces`.
pub fn get_declared_marketplaces() -> serde_json::Map<String, serde_json::Value> {
    use super::official_marketplace::{OFFICIAL_MARKETPLACE_NAME, OFFICIAL_MARKETPLACE_SOURCE};
    let settings = crate::utils::settings::get_initial_settings();
    let mut enabled = super::add_dir_plugin_settings::get_add_dir_enabled_plugins();
    if let Some(serde_json::Value::Object(entries)) = settings.enabled_plugins {
        enabled.extend(entries);
    }
    let mut result = serde_json::Map::new();
    for (id, value) in enabled {
        if value.as_bool().unwrap_or(false)
            && super::plugin_identifier::parse_plugin_identifier(&id)
                .marketplace
                .as_deref()
                == Some(OFFICIAL_MARKETPLACE_NAME)
        {
            result.insert(
                OFFICIAL_MARKETPLACE_NAME.into(),
                serde_json::json!({"source":&*OFFICIAL_MARKETPLACE_SOURCE,"sourceIsFallback":true}),
            );
            break;
        }
    }
    result.extend(super::add_dir_plugin_settings::get_add_dir_extra_marketplaces());
    if let Some(serde_json::Value::Object(entries)) = settings.extra_known_marketplaces {
        result.extend(entries);
    }
    result
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:200-216#getMarketplaceDeclaringSource`.
pub fn get_marketplace_declaring_source(
    name: &str,
) -> Option<crate::utils::settings::constants::SettingSource> {
    use crate::utils::settings::constants::SettingSource;
    [
        SettingSource::Local,
        SettingSource::Project,
        SettingSource::User,
    ]
    .into_iter()
    .find(|&source| {
        crate::utils::settings::get_settings_for_source(source)
            .and_then(|s| s.extra_known_marketplaces)
            .is_some_and(|s| s.get(name).is_some_and(|v| !v.is_null()))
    })
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:226-238#saveMarketplaceToSettings`.
/// Callers pass User for the source's omitted/default argument. Native settings
/// write failures use its existing Result carrier (source ignores returned error).
pub fn save_marketplace_to_settings(
    name: &str,
    entry: &DeclaredMarketplace,
    setting_source: crate::utils::settings::constants::SettingSource,
) -> anyhow::Result<()> {
    let mut current = crate::utils::settings::get_settings_for_source(setting_source)
        .and_then(|s| s.extra_known_marketplaces)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    current.insert(name.into(), entry.clone());
    let _ = crate::utils::settings::update_settings_for_source(
        setting_source,
        &serde_json::json!({"extraKnownMarketplaces":current})
            .as_object()
            .unwrap(),
    );
    Ok(())
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:327-350#saveKnownMarketplacesConfig`.
pub fn save_known_marketplaces_config(
    config: KnownMarketplacesConfig,
) -> impl std::future::Future<Output = anyhow::Result<()>> {
    let prepared = (|| -> anyhow::Result<_> {
        let value = serde_json::Value::Object(config);
        let config_file = get_known_marketplaces_file();
        let parsed =
            crate::utils::zod::safe_parse(super::schemas::known_marketplaces_file_schema(), &value)
                .map_err(|error| {
                    crate::utils::errors::ConfigParseError::new(
                        format!("Invalid marketplace config: {}", error.message()),
                        config_file.to_string_lossy(),
                        value,
                    )
                })?;
        let fs = crate::utils::fs_operations::get_fs_implementation();
        let pending = fs.mkdir(&marketplace_path!(&config_file, ".."), None);
        Ok((config_file, parsed, pending))
    })();
    async move {
        let (config_file, parsed, pending) = prepared?;
        pending.await?;
        crate::utils::slow_operations::write_file_sync_deprecated(
            &config_file,
            &crate::utils::slow_operations::json_stringify(&parsed, 2),
            true,
        )?;
        Ok(())
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:380-434#registerSeedMarketplaces`.
pub async fn register_seed_marketplaces() -> anyhow::Result<bool> {
    let dirs = super::plugin_directories::get_plugin_seed_dirs();
    if dirs.is_empty() {
        return Ok(false);
    }
    let mut primary = load_known_marketplaces_config().await?;
    let mut claimed = std::collections::HashSet::new();
    let mut changed = 0;
    for dir in dirs {
        let Some(config) = read_seed_known_marketplaces(&dir).await else {
            continue;
        };
        for (name, entry) in crate::utils::process_env::ecmascript_object_entries(&config) {
            if claimed.contains(name) {
                continue;
            }
            let Some(location) = find_seed_marketplace_location(&dir, name).await else {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "Seed marketplace '{name}' not found under {}/marketplaces/, skipping",
                        dir.display()
                    ),
                    crate::utils::debug::DebugLogLevel::Warn,
                );
                continue;
            };
            claimed.insert(name.to_owned());
            let desired = serde_json::json!({"source":entry["source"],"installLocation":location,"lastUpdated":entry["lastUpdated"],"autoUpdate":false});
            if primary.get(name) == Some(&desired) {
                continue;
            }
            primary.insert(name.to_owned(), desired);
            changed += 1;
        }
    }
    if changed > 0 {
        save_known_marketplaces_config(primary).await?;
        crate::utils::debug::log_for_debugging(&format!(
            "Synced {changed} marketplace(s) from seed dir(s)"
        ));
        return Ok(true);
    }
    Ok(false)
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:436-462#readSeedKnownMarketplaces`.
fn read_seed_known_marketplaces(
    seed_dir: &Path,
) -> impl std::future::Future<Output = Option<KnownMarketplacesConfig>> {
    let pending = crate::utils::fs_operations::get_fs_implementation().read_file(
        &marketplace_path!(seed_dir, "known_marketplaces.json"),
        crate::utils::fs_operations::BufferEncoding::Utf8,
    );
    async move {
        let read: anyhow::Result<Option<KnownMarketplacesConfig>> = async {
            let bytes = pending.await?.to_string_lossy().into_bytes();
            let value =
                crate::utils::slow_operations::json_parse(&String::from_utf8_lossy(&bytes))?
                    .to_json();
            match crate::utils::zod::safe_parse(
                super::schemas::known_marketplaces_file_schema(),
                &value,
            ) {
                Ok(value) => Ok(value.as_object().cloned()),
                Err(error) => {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Seed known_marketplaces.json invalid at {}: {}",
                            seed_dir.display(),
                            error.message()
                        ),
                        crate::utils::debug::DebugLogLevel::Warn,
                    );
                    Ok(None)
                }
            }
        }
        .await;
        match read {
            Ok(v) => v,
            Err(error) => {
                if !crate::utils::errors::is_enoent(&error) {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Failed to read seed known_marketplaces.json at {}: {error}",
                            seed_dir.display()
                        ),
                        crate::utils::debug::DebugLogLevel::Warn,
                    );
                }
                None
            }
        }
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:473-488#findSeedMarketplaceLocation`.
async fn find_seed_marketplace_location(seed_dir: &Path, name: &str) -> Option<PathBuf> {
    for candidate in [
        marketplace_path!(seed_dir, "marketplaces", name),
        marketplace_path!(seed_dir, "marketplaces", format!("{name}.json")),
    ] {
        if read_cached_marketplace(&candidate).await.is_ok() {
            return Some(candidate);
        }
    }
    None
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:496-500#seedDirFor`.
fn seed_dir_for(install_location: &str) -> Option<PathBuf> {
    super::plugin_directories::get_plugin_seed_dirs()
        .into_iter()
        .find(|d| {
            let d = d.to_string_lossy();
            install_location == d
                || install_location.starts_with(&format!("{d}{}", std::path::MAIN_SEPARATOR))
        })
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1186-1192#redactHeaders`.
fn redact_headers(headers: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Object(
        headers
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    serde_json::Value::String("***REDACTED***".into()),
                )
            })
            .collect(),
    )
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:1256-1350#cacheMarketplaceFromUrl`.
/// Reqwest carries Axios I/O; native transport/proxy/TLS/error wording remains
/// the explicit shared transport seam. HTTP/status/schema/order are source-owned.
fn cache_marketplace_from_url(
    url: &str,
    cache_path: &Path,
    custom_headers: Option<&serde_json::Map<String, serde_json::Value>>,
    on_progress: Option<&MarketplaceProgressCallback<'_>>,
) -> impl std::future::Future<Output = anyhow::Result<()>> {
    use super::fetch_telemetry::{
        PluginFetchOutcome, PluginFetchSource, classify_fetch_error, log_plugin_fetch,
    };
    let fs = crate::utils::fs_operations::get_fs_implementation();
    async move {
        let redacted = redact_url_credentials(url);
        safe_call_progress(
            on_progress,
            &format!("Downloading marketplace from {redacted}"),
        );
        crate::utils::debug::log_for_debugging(&format!(
            "Downloading marketplace from URL: {redacted}"
        ));
        if let Some(headers) = custom_headers.filter(|h| !h.is_empty()) {
            crate::utils::debug::log_for_debugging(&format!(
                "Using custom headers: {}",
                crate::utils::slow_operations::json_stringify(&redact_headers(headers), 0)
            ));
        }
        let started = std::time::Instant::now();
        let fetched:anyhow::Result<serde_json::Value>=async {
        // CC `axios.get` (`marketplaceManager.ts:1282-1285`), through the
        // global interceptor.
        let client=crate::utils::proxy::create_axios_instance()?.timeout(std::time::Duration::from_secs(10)).build()?;
        let mut headers=reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT,reqwest::header::HeaderValue::from_static("application/json, text/plain, */*"));
        if let Some(custom_headers)=custom_headers {for(key,value)in custom_headers {headers.insert(reqwest::header::HeaderName::from_bytes(key.as_bytes())?,reqwest::header::HeaderValue::from_str(value.as_str().unwrap_or(""))?);}}
        headers.insert(reqwest::header::USER_AGENT,reqwest::header::HeaderValue::from_static("Claude-Code-Plugin-Manager"));
        let request=client.get(url).headers(headers);
        let response=request.send().await.map_err(|error|{let message=if error.is_connect(){format!("Could not connect to {redacted}. Please check your internet connection and verify the URL is correct.\n\nTechnical details: {error}")}else{format!("Failed to download marketplace from {redacted}: {error}")};anyhow::anyhow!(message)})?;
        let status=response.status();if !status.is_success(){anyhow::bail!("HTTP {} error while downloading marketplace from {redacted}. The marketplace file may not exist at this URL.\n\nTechnical details: Request failed with status code {}",status.as_u16(),status.as_u16());}
        let text=response.text().await?;Ok(crate::utils::slow_operations::json_parse(&text).map(|v|v.to_json()).unwrap_or(serde_json::Value::String(text)))
    }.await;
        let data = match fetched {
            Ok(v) => v,
            Err(error) => {
                log_plugin_fetch(
                    PluginFetchSource::MarketplaceUrl,
                    Some(url),
                    PluginFetchOutcome::Failure,
                    started.elapsed().as_secs_f64() * 1000.0,
                    Some(classify_fetch_error(&error)),
                );
                return Err(error);
            }
        };
        safe_call_progress(on_progress, "Validating marketplace data");
        let parsed =
            crate::utils::zod::safe_parse(super::schemas::plugin_marketplace_schema(), &data)
                .map_err(|error| {
                    log_plugin_fetch(
                        PluginFetchSource::MarketplaceUrl,
                        Some(url),
                        PluginFetchOutcome::Failure,
                        started.elapsed().as_secs_f64() * 1000.0,
                        Some("invalid_schema"),
                    );
                    let issues = error
                        .issues
                        .iter()
                        .map(|e| {
                            format!(
                                "{}: {}",
                                e.path
                                    .iter()
                                    .map(|p| match p {
                                        crate::utils::zod::PathSegment::Key(k) => k.clone(),
                                        crate::utils::zod::PathSegment::Index(i) => i.to_string(),
                                    })
                                    .collect::<Vec<_>>()
                                    .join("."),
                                e.message
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    crate::utils::errors::ConfigParseError::new(
                        format!("Invalid marketplace schema from URL: {issues}"),
                        &redacted,
                        data,
                    )
                })?;
        log_plugin_fetch(
            PluginFetchSource::MarketplaceUrl,
            Some(url),
            PluginFetchOutcome::Success,
            started.elapsed().as_secs_f64() * 1000.0,
            None,
        );
        safe_call_progress(on_progress, "Saving marketplace to cache");
        fs.mkdir(&marketplace_path!(cache_path, ".."), None).await?;
        crate::utils::slow_operations::write_file_sync_deprecated(
            cache_path,
            &crate::utils::slow_operations::json_stringify(&parsed, 2),
            true,
        )?;
        Ok(())
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:1355-1367#getCachePathForSource`.
fn get_cache_path_for_source(source: &serde_json::Value) -> String {
    match source["source"].as_str().unwrap_or("") {
        "github" => source["repo"].as_str().unwrap_or("").replacen('/', "-", 1),
        "npm" => source["package"]
            .as_str()
            .unwrap_or("")
            .replacen('@', "", 1)
            .replacen('/', "-", 1),
        "file" | "directory" => {
            // Node basename preserves final dot/dot-dot segments; Rust
            // Path::file_name normalizes them away before observation.
            let path = source["path"].as_str().unwrap_or("");
            let base = path
                .trim_end_matches(std::path::MAIN_SEPARATOR)
                .rsplit(std::path::MAIN_SEPARATOR)
                .next()
                .unwrap_or("");
            if source["source"] == "file" {
                base.replacen(".json", "", 1)
            } else {
                base.to_owned()
            }
        }
        _ => format!("temp_{}", chrono::Utc::now().timestamp_millis()),
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:1433-1768#loadAndCacheMarketplace`.
pub fn load_and_cache_marketplace(
    source: &serde_json::Value,
    on_progress: Option<&MarketplaceProgressCallback<'_>>,
) -> impl std::future::Future<Output = anyhow::Result<LoadedPluginMarketplace>> {
    use super::schemas::is_local_marketplace_source;
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let cache_dir = get_marketplaces_cache_dir();
    let pending = fs.mkdir(&cache_dir, None);
    async move {
        pending.await?;
        let temp_name = get_cache_path_for_source(source);
        let mut temporary_cache_path = PathBuf::new();
        let mut cleanup_needed = false;
        let loaded:anyhow::Result<LoadedPluginMarketplace>=async {
        let marketplace_path;
        match source["source"].as_str().unwrap_or("") {
            "url"=>{temporary_cache_path=marketplace_path!(&cache_dir,format!("{temp_name}.json"));cleanup_needed=true;cache_marketplace_from_url(source["url"].as_str().unwrap_or(""),&temporary_cache_path,source["headers"].as_object(),on_progress).await?;marketplace_path=temporary_cache_path.clone();}
            "github"=>{
                let repo=source["repo"].as_str().unwrap_or("");let ssh_url=format!("git@github.com:{repo}.git");let https_url=format!("https://github.com/{repo}.git");temporary_cache_path=marketplace_path!(&cache_dir,&temp_name);cleanup_needed=true;
                let sparse=source["sparsePaths"].as_array().map(|a|a.iter().map(|v|v.as_str().unwrap_or("").to_owned()).collect::<Vec<_>>());let git_ref=source["ref"].as_str();let ssh=is_github_ssh_likely_configured().await;
                let (first,second)=if ssh{(&ssh_url,&https_url)}else{(&https_url,&ssh_url)};
                safe_call_progress(on_progress,&if ssh {format!("Cloning via SSH: {ssh_url}")}else{format!("SSH not configured, cloning via HTTPS: {https_url}")});
                if !ssh{crate::utils::debug::log_for_debugging_with_level(&format!("SSH not configured for GitHub, using HTTPS for {repo}"),crate::utils::debug::DebugLogLevel::Info);}
                if let Err(error)=cache_marketplace_from_git(first,&temporary_cache_path,git_ref,sparse.as_deref(),on_progress,false).await {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));
                    safe_call_progress(on_progress,&if ssh{format!("SSH clone failed, retrying with HTTPS: {https_url}")}else{format!("HTTPS clone failed, retrying with SSH: {ssh_url}")});
                    crate::utils::debug::log_for_debugging_with_level(&if ssh{format!("SSH clone failed for {repo} despite SSH being configured, falling back to HTTPS")}else{format!("HTTPS clone failed for {repo} ({error}), falling back to SSH")},crate::utils::debug::DebugLogLevel::Info);
                    fs.rm(&temporary_cache_path, crate::utils::fs_operations::RmOptions { recursive: true, force: true }).await?;
                    if let Err(error)=cache_marketplace_from_git(second,&temporary_cache_path,git_ref,sparse.as_deref(),on_progress,false).await{crate::utils::log::log_error(crate::utils::log::LogError::new(error.to_string()));return Err(error);}
                }
                marketplace_path=marketplace_path!(&temporary_cache_path,source["path"].as_str().filter(|s|!s.is_empty()).unwrap_or(".claude-plugin/marketplace.json"));
            }
            "git"=>{
                temporary_cache_path=marketplace_path!(&cache_dir,&temp_name);cleanup_needed=true;let sparse=source["sparsePaths"].as_array().map(|a|a.iter().map(|v|v.as_str().unwrap_or("").to_owned()).collect::<Vec<_>>());
                cache_marketplace_from_git(source["url"].as_str().unwrap_or(""),&temporary_cache_path,source["ref"].as_str(),sparse.as_deref(),on_progress,false).await?;
                marketplace_path=marketplace_path!(&temporary_cache_path,source["path"].as_str().filter(|s|!s.is_empty()).unwrap_or(".claude-plugin/marketplace.json"));
            }
            "npm"=>anyhow::bail!("NPM marketplace sources not yet implemented"),
            "file"=>{marketplace_path=marketplace_resolve!(source["path"].as_str().unwrap_or(""));temporary_cache_path=marketplace_path!(marketplace_path!(marketplace_path.clone(),".."),"..");}
            "directory"=>{temporary_cache_path=marketplace_resolve!(source["path"].as_str().unwrap_or(""));marketplace_path=marketplace_path!(&temporary_cache_path,".claude-plugin","marketplace.json");}
            "settings"=>{temporary_cache_path=marketplace_path!(&cache_dir,source["name"].as_str().unwrap_or(""));marketplace_path=marketplace_path!(&temporary_cache_path,".claude-plugin","marketplace.json");fs.mkdir(&marketplace_path!(&marketplace_path,".."), None).await?;let owner=source.get("owner").filter(|v|!v.is_null()).cloned().unwrap_or(serde_json::json!({"name":"settings"}));tokio::fs::write(&marketplace_path,crate::utils::slow_operations::json_stringify(&serde_json::json!({"name":source["name"],"owner":owner,"plugins":source["plugins"]}),2)).await?;}
            _=>anyhow::bail!("Unsupported marketplace source type"),
        }
        crate::utils::debug::log_for_debugging(&format!("Reading marketplace from {}",marketplace_path.display()));
        let marketplace=parse_file_with_schema(&marketplace_path,super::schemas::plugin_marketplace_schema()).await.map_err(|e|if crate::utils::errors::is_enoent(&e){anyhow::anyhow!("Marketplace file not found at {}",marketplace_path.display())}else{anyhow::anyhow!("Failed to parse marketplace file at {}: {e}",marketplace_path.display())})?;
        let name=marketplace["name"].as_str().unwrap();let final_path=marketplace_path!(&cache_dir,name);let resolved_final=marketplace_resolve!(&final_path);let resolved_dir=marketplace_resolve!(&cache_dir);
        if resolved_final==resolved_dir||!resolved_final.starts_with(&resolved_dir){anyhow::bail!("Marketplace name '{name}' resolves to a path outside the cache directory");}
        if temporary_cache_path!=final_path&&!is_local_marketplace_source(source){
            let finalize:anyhow::Result<()>=async{safe_call_progress(on_progress,"Cleaning up old marketplace cache…");fs.rm(&final_path, crate::utils::fs_operations::RmOptions { recursive: true, force: true }).await?;fs.rename(&temporary_cache_path,&final_path).await?;Ok(())}.await;
            finalize.map_err(|e|anyhow::anyhow!("Failed to finalize marketplace cache. Please manually delete the directory at {} if it exists and try again.\n\nTechnical details: {e}",final_path.display()))?;temporary_cache_path=final_path;cleanup_needed=false;
        }
        Ok(LoadedPluginMarketplace{marketplace,cache_path:temporary_cache_path.clone()})
    }.await;
        if loaded.is_err()
            && cleanup_needed
            && !temporary_cache_path.as_os_str().is_empty()
            && !is_local_marketplace_source(source)
        {
            if let Err(error) = fs
                .rm(
                    &temporary_cache_path,
                    crate::utils::fs_operations::RmOptions {
                        recursive: true,
                        force: true,
                    },
                )
                .await
            {
                crate::utils::debug::log_for_debugging_with_level(
                    &format!(
                        "Warning: Failed to clean up temporary marketplace cache at {}: {error}",
                        temporary_cache_path.display()
                    ),
                    crate::utils::debug::DebugLogLevel::Warn,
                );
            }
        }
        loaded
    }
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:1787-1791` add result.
pub struct AddedMarketplaceSource {
    pub name: String,
    pub already_materialized: bool,
    pub resolved_source: serde_json::Value,
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:1782-1924#addMarketplaceSource`.
pub async fn add_marketplace_source(
    source: &serde_json::Value,
    on_progress: Option<&MarketplaceProgressCallback<'_>>,
) -> anyhow::Result<AddedMarketplaceSource> {
    use super::marketplace_helpers::*;
    use super::schemas::is_local_marketplace_source;
    let mut source = source.clone();
    if is_local_marketplace_source(&source)
        && !Path::new(source["path"].as_str().unwrap_or("")).is_absolute()
    {
        source["path"] = marketplace_resolve!(source["path"].as_str().unwrap_or(""))
            .to_string_lossy()
            .into_owned()
            .into();
    }
    if !is_source_allowed_by_policy(&source) {
        let display = format_source_for_display(&source);
        if is_source_in_blocklist(&source) {
            anyhow::bail!("Marketplace source '{display}' is blocked by enterprise policy.");
        }
        let allowlist = get_strict_known_marketplaces().unwrap_or_default();
        let host_patterns = get_host_patterns_from_allowlist();
        let mut message = format!("Marketplace source '{display}'");
        if let Some(host) = extract_host_from_source(&source).filter(|s| !s.is_empty()) {
            message.push_str(&format!(" ({host})"));
        }
        message.push_str(" is blocked by enterprise policy.");
        if allowlist.is_empty() {
            message.push_str(" No external marketplaces are allowed.");
        } else {
            message.push_str(&format!(
                " Allowed sources: {}",
                allowlist
                    .iter()
                    .map(format_source_for_display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if source["source"] == "github" && !host_patterns.is_empty() {
            let repo = source["repo"].as_str().unwrap_or("");
            message.push_str(&format!("\n\nTip: The shorthand \"{repo}\" assumes github.com. For internal GitHub Enterprise, use the full URL:\n  git@your-github-host.com:{repo}.git"));
        }
        anyhow::bail!(message);
    }
    let existing_config = load_known_marketplaces_config().await?;
    for (name, entry) in crate::utils::process_env::ecmascript_object_entries(&existing_config) {
        if entry["source"] == source {
            crate::utils::debug::log_for_debugging(&format!(
                "Source already materialized as '{name}', skipping clone"
            ));
            return Ok(AddedMarketplaceSource {
                name: name.to_owned(),
                already_materialized: true,
                resolved_source: source,
            });
        }
    }
    let loaded = load_and_cache_marketplace(&source, on_progress).await?;
    let name = loaded.marketplace["name"].as_str().unwrap().to_owned();
    if let Some(error) = super::schemas::validate_official_name_source(&name, &source) {
        anyhow::bail!(error);
    }
    let mut config = load_known_marketplaces_config().await?;
    if let Some(old) = config.get(&name) {
        let location = old["installLocation"].as_str().unwrap_or("");
        if let Some(seed) = seed_dir_for(location) {
            anyhow::bail!(
                "Marketplace '{name}' is seed-managed ({}). To use a different source, ask your admin to update the seed, or use a different marketplace name.",
                seed.display()
            );
        }
        crate::utils::debug::log_for_debugging(&format!(
            "Marketplace '{name}' exists with different source — overwriting"
        ));
        if !is_local_marketplace_source(&old["source"]) {
            let cache = marketplace_resolve!(get_marketplaces_cache_dir());
            let resolved_old = marketplace_resolve!(location);
            let resolved_new = marketplace_resolve!(&loaded.cache_path);
            if resolved_old != resolved_new {
                if resolved_old.starts_with(&cache) {
                    crate::utils::fs_operations::rm(Path::new(location), true, true).await?;
                } else {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Skipping cleanup of old installLocation ({location}) — outside {}. The path is corrupted; leaving it alone and overwriting the config entry.",
                            cache.display()
                        ),
                        crate::utils::debug::DebugLogLevel::Warn,
                    );
                }
            }
        }
    }
    config.insert(name.clone(),serde_json::json!({"source":source,"installLocation":loaded.cache_path,"lastUpdated":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true)}));
    save_known_marketplaces_config(config).await?;
    crate::utils::debug::log_for_debugging(&format!("Added marketplace source: {name}"));
    Ok(AddedMarketplaceSource {
        name,
        already_materialized: false,
        resolved_source: source,
    })
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:1937-2049#removeMarketplaceSource`.
pub async fn remove_marketplace_source(name: &str) -> anyhow::Result<()> {
    let mut config = load_known_marketplaces_config().await?;
    let entry = config
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Marketplace '{name}' not found"))?;
    if let Some(seed) = seed_dir_for(entry["installLocation"].as_str().unwrap_or("")) {
        anyhow::bail!(
            "Marketplace '{name}' is registered from the read-only seed directory ({}) and will be re-registered on next startup. To stop using its plugins: claude plugin disable <plugin>@{name}",
            seed.display()
        );
    }
    config.shift_remove(name);
    save_known_marketplaces_config(config).await?;
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let dir = get_marketplaces_cache_dir();
    fs.rm(
        &marketplace_path!(&dir, name),
        crate::utils::fs_operations::RmOptions {
            recursive: true,
            force: true,
        },
    )
    .await?;
    fs.rm(
        &marketplace_path!(&dir, format!("{name}.json")),
        crate::utils::fs_operations::RmOptions {
            recursive: false,
            force: true,
        },
    )
    .await?;
    use crate::utils::settings::constants::SettingSource;
    for (source, label) in [
        (SettingSource::User, "userSettings"),
        (SettingSource::Project, "projectSettings"),
        (SettingSource::Local, "localSettings"),
    ] {
        let Some(settings) = crate::utils::settings::get_settings_for_source(source) else {
            continue;
        };
        let mut updates = serde_json::Map::new();
        if let Some(serde_json::Value::Object(mut entries)) = settings.extra_known_marketplaces {
            if entries.contains_key(name) {
                entries.insert(name.into(), serde_json::Value::Null);
                updates.insert("extraKnownMarketplaces".into(), entries.into());
            }
        }
        if let Some(serde_json::Value::Object(mut plugins)) = settings.enabled_plugins {
            let mut removed = false;
            for (id, value) in &mut plugins {
                if id.ends_with(&format!("@{name}")) {
                    *value = serde_json::Value::Null;
                    removed = true;
                }
            }
            if removed {
                updates.insert("enabledPlugins".into(), plugins.into());
            }
        }
        if !updates.is_empty() {
            match crate::utils::settings::update_settings_for_source(source, &updates) {
                Ok(()) => crate::utils::debug::log_for_debugging(&format!(
                    "Cleaned up marketplace '{name}' from {label} settings"
                )),
                Err(error) => {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                    crate::utils::debug::log_for_debugging(&format!(
                        "Failed to clean up marketplace '{name}' from {label} settings: {error}"
                    ));
                }
            }
        }
    }
    let removed = super::installed_plugins_manager::remove_all_plugins_for_marketplace(name)?;
    for path in removed.orphaned_paths {
        super::cache_utils::mark_plugin_version_orphaned(Path::new(&path)).await;
    }
    for id in removed.removed_plugin_ids {
        super::plugin_options_storage::delete_plugin_options(&id);
        super::plugin_directories::delete_plugin_data_dir(&id).await;
    }
    crate::utils::debug::log_for_debugging(&format!("Removed marketplace source: {name}"));
    Ok(())
}

/// Maps to: CC `utils/plugins/marketplaceManager.ts:2122-2178#getMarketplace`
/// lodash memoize Promise carrier. Shared result retains success identity and
/// rejection; native Error identity and its cause chain remain shared via Arc.
type MarketplaceMemo = futures::future::Shared<
    futures::future::BoxFuture<
        'static,
        Result<std::sync::Arc<serde_json::Value>, std::sync::Arc<anyhow::Error>>,
    >,
>;
/// Native cloneable rejection carrier for CC getMarketplace's memoized Promise.
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2122-2178#getMarketplace`.
#[derive(Debug, Clone)]
struct SharedMarketplaceError(std::sync::Arc<anyhow::Error>);
impl std::fmt::Display for SharedMarketplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), f)
    }
}
impl std::error::Error for SharedMarketplaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2122-2178#getMarketplace.cache`.
static MARKETPLACE_MEMO: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, MarketplaceMemo>>,
> = std::sync::LazyLock::new(Default::default);
/// Maps to: CC `utils/plugins/marketplaceManager.ts:122-124#clearMarketplacesCache`.
pub fn clear_marketplaces_cache() {
    MARKETPLACE_MEMO.lock().unwrap().clear();
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2122-2178#getMarketplace`.
/// PORTING A6/A7: eager Promise work survives observer cancellation and cache
/// deletion on the process runtime; the map never owns task cancellation.
pub fn get_marketplace(
    name: &str,
) -> impl std::future::Future<Output = anyhow::Result<std::sync::Arc<serde_json::Value>>> + Send + 'static
{
    use futures::FutureExt;
    let future = {
        let mut cache = MARKETPLACE_MEMO.lock().unwrap();
        cache.entry(name.into()).or_insert_with(||{
        let owned_name=name.to_owned();
        let future=async move {let name=owned_name.as_str();
        let loaded:anyhow::Result<serde_json::Value>=async {
            let mut config=load_known_marketplaces_config().await?;let entry=config.get(name).cloned().ok_or_else(||anyhow::anyhow!("Marketplace '{name}' not found in configuration. Available marketplaces: {}",crate::utils::process_env::ecmascript_object_entries(&config).into_iter().map(|(name,_)|name).collect::<Vec<_>>().join(", ")))?;let source=&entry["source"];
            if super::schemas::is_local_marketplace_source(source)&&!Path::new(source["path"].as_str().unwrap_or("")).is_absolute(){anyhow::bail!("Marketplace \"{name}\" has a relative source path ({}) in known_marketplaces.json — this is stale state from an older Claude Code version. Run 'claude marketplace remove {name}' and re-add it from the original project directory.",source["path"].as_str().unwrap_or(""));}
            match read_cached_marketplace(Path::new(entry["installLocation"].as_str().unwrap_or(""))).await{Ok(data)=>return Ok(data),Err(error)=>crate::utils::debug::log_for_debugging_with_level(&format!("Cache corrupted or missing for marketplace {name}, re-fetching from source: {error}"),crate::utils::debug::DebugLogLevel::Warn)}
            let loaded=load_and_cache_marketplace(source,None).await.map_err(|error|anyhow::anyhow!("Failed to load marketplace \"{name}\" from source ({}): {error}",source["source"].as_str().unwrap_or("")))?;
            config.get_mut(name).unwrap()["lastUpdated"]=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true).into();save_known_marketplaces_config(config).await?;Ok(loaded.marketplace)
        }.await;loaded.map(std::sync::Arc::new).map_err(std::sync::Arc::new)

        }.boxed().shared();
        let worker=future.clone();
        crate::utils::process_runtime::runtime_handle_for_detached_work().expect("marketplace memo requires process lifetime runtime").spawn(async move {let _=worker.await;});
        future
    }).clone()
    };
    async move {
        future
            .await
            .map_err(|error| anyhow::Error::new(SharedMarketplaceError(error)))
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2238-2280#getPluginById`.
pub async fn get_plugin_by_id(plugin_id: &str) -> Option<MarketplacePluginMetadata> {
    if let Some(cached) = get_plugin_by_id_cache_only(plugin_id).await {
        return Some(cached);
    }
    let parsed = super::plugin_identifier::parse_plugin_identifier(plugin_id);
    let marketplace_name = parsed.marketplace.as_deref().filter(|s| !s.is_empty())?;
    if parsed.name.is_empty() {
        return None;
    }
    let fetched: anyhow::Result<Option<MarketplacePluginMetadata>> = async {
        let config = load_known_marketplaces_config().await?;
        let Some(entry) = config.get(marketplace_name) else {
            return Ok(None);
        };
        let marketplace = get_marketplace(marketplace_name).await?;
        Ok(marketplace["plugins"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == parsed.name))
            .map(|p| MarketplacePluginMetadata {
                entry: p.clone(),
                marketplace_install_location: entry.get("installLocation").cloned(),
            }))
    }
    .await;
    match fetched {
        Ok(v) => v,
        Err(error) => {
            crate::utils::debug::log_for_debugging_with_level(
                &format!("Could not find plugin {plugin_id}: {error}"),
                crate::utils::debug::DebugLogLevel::Debug,
            );
            None
        }
    }
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2296-2351#refreshAllMarketplaces`.
pub async fn refresh_all_marketplaces() -> anyhow::Result<()> {
    let mut config = load_known_marketplaces_config().await?;
    let names = crate::utils::process_env::ecmascript_object_entries(&config)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect::<Vec<_>>();
    for name in names {
        let entry = config
            .get_mut(&name)
            .expect("source Object.entries snapshot");
        if seed_dir_for(entry["installLocation"].as_str().unwrap_or("")).is_some() {
            crate::utils::debug::log_for_debugging(&format!(
                "Skipping seed-managed marketplace '{name}' in bulk refresh"
            ));
            continue;
        }
        if entry["source"]["source"] == "settings" {
            continue;
        }
        if name == super::official_marketplace::OFFICIAL_MARKETPLACE_NAME {
            if super::official_marketplace_gcs::fetch_official_marketplace_from_gcs(
                Path::new(entry["installLocation"].as_str().unwrap_or("")),
                &get_marketplaces_cache_dir(),
            )
            .await
            .is_some()
            {
                entry["lastUpdated"] = chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into();
                continue;
            }
            if !crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale(
                "tengu_plugin_official_mkt_git_fallback",
                true,
            ) {
                crate::utils::debug::log_for_debugging(
                    "Skipping official marketplace bulk refresh: GCS failed, git fallback disabled",
                );
                continue;
            }
        }
        match load_and_cache_marketplace(&entry["source"], None).await {
            Ok(loaded) => {
                entry["lastUpdated"] = chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into();
                entry["installLocation"] = loaded.cache_path.to_string_lossy().into_owned().into();
            }
            Err(error) => crate::utils::debug::log_for_debugging_with_level(
                &format!("Failed to refresh marketplace {name}: {error}"),
                crate::utils::debug::DebugLogLevel::Error,
            ),
        }
    }
    save_known_marketplaces_config(config).await
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2365-2575#refreshMarketplace`.
pub async fn refresh_marketplace(
    name: &str,
    on_progress: Option<&MarketplaceProgressCallback<'_>>,
    disable_credential_helper: bool,
) -> anyhow::Result<()> {
    let mut config = load_known_marketplaces_config().await?;
    let entry = config.get(name).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Marketplace '{name}' not found. Available marketplaces: {}",
            crate::utils::process_env::ecmascript_object_entries(&config)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    MARKETPLACE_MEMO.lock().unwrap().remove(name);
    if entry["source"]["source"] == "settings" {
        crate::utils::debug::log_for_debugging(&format!(
            "Skipping refresh for settings-sourced marketplace '{name}' — no upstream"
        ));
        return Ok(());
    }
    let refreshed:anyhow::Result<()>=async {
        let location=entry["installLocation"].as_str().unwrap_or("");let path=Path::new(location);let source=&entry["source"];
        if let Some(seed)=seed_dir_for(location){anyhow::bail!("Marketplace '{name}' is seed-managed ({}) and its content is controlled by the seed image. To update: ask your admin to update the seed.",seed.display());}
        if !super::schemas::is_local_marketplace_source(source){let cache=marketplace_resolve!(get_marketplaces_cache_dir());let resolved=marketplace_resolve!(path);if !resolved.starts_with(&cache){anyhow::bail!("Marketplace '{name}' has a corrupted installLocation ({location}) — expected a path inside {}. This can happen after cross-platform path writes or manual edits to known_marketplaces.json. Run: claude plugin marketplace remove \"{name}\" and re-add it.",cache.display());}}
        if name==super::official_marketplace::OFFICIAL_MARKETPLACE_NAME {
            if super::official_marketplace_gcs::fetch_official_marketplace_from_gcs(path,&get_marketplaces_cache_dir()).await.is_some(){config.get_mut(name).unwrap()["lastUpdated"]=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true).into();save_known_marketplaces_config(config.clone()).await?;return Ok(());}
            if !crate::services::analytics::growthbook::get_feature_value_cached_may_be_stale("tengu_plugin_official_mkt_git_fallback",true){anyhow::bail!("Official marketplace GCS fetch failed and git fallback is disabled");}crate::utils::debug::log_for_debugging_with_level("Official marketplace GCS failed; falling back to git",crate::utils::debug::DebugLogLevel::Warn);
        }
        match source["source"].as_str().unwrap_or(""){
            "github"|"git"=>{
                let sparse=source["sparsePaths"].as_array().map(|a|a.iter().map(|v|v.as_str().unwrap_or("").to_owned()).collect::<Vec<_>>());let git_ref=source["ref"].as_str();
                if source["source"]=="github"{let repo=source["repo"].as_str().unwrap_or("");let ssh=format!("git@github.com:{repo}.git");let https=format!("https://github.com/{repo}.git");
                    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref()){cache_marketplace_from_git(&https,path,git_ref,sparse.as_deref(),on_progress,disable_credential_helper).await?;}
                    else{let configured=is_github_ssh_likely_configured().await;let(first,second)=if configured{(&ssh,&https)}else{(&https,&ssh)};if cache_marketplace_from_git(first,path,git_ref,sparse.as_deref(),on_progress,disable_credential_helper).await.is_err(){crate::utils::debug::log_for_debugging_with_level(&format!("Marketplace refresh failed with {} for {repo}, falling back to {}",if configured{"SSH"}else{"HTTPS"},if configured{"HTTPS"}else{"SSH"}),crate::utils::debug::DebugLogLevel::Info);cache_marketplace_from_git(second,path,git_ref,sparse.as_deref(),on_progress,disable_credential_helper).await?;}}
                }else{cache_marketplace_from_git(source["url"].as_str().unwrap_or(""),path,git_ref,sparse.as_deref(),on_progress,disable_credential_helper).await?;}
                if read_cached_marketplace(path).await.is_err(){let display=if source["source"]=="github"{source["repo"].as_str().unwrap_or("").to_owned()}else{redact_url_credentials(source["url"].as_str().unwrap_or(""))};let reason=if name=="claude-code-plugins"{"We've deprecated \"claude-code-plugins\" in favor of \"claude-plugins-official\"."}else{"This marketplace may have been deprecated or moved to a new location."};anyhow::bail!("The marketplace.json file is no longer present in this repository.\n\n{reason}\nSource: {display}\n\nYou can remove this marketplace with: claude plugin marketplace remove \"{name}\"");}
            }
            "url"=>cache_marketplace_from_url(source["url"].as_str().unwrap_or(""),path,source["headers"].as_object(),on_progress).await?,
            "file"|"directory"=>{safe_call_progress(on_progress,"Validating local marketplace");read_cached_marketplace(path).await?;},
            _=>anyhow::bail!("Unsupported marketplace source type for refresh"),
        }
        config.get_mut(name).unwrap()["lastUpdated"]=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true).into();save_known_marketplaces_config(config.clone()).await?;crate::utils::debug::log_for_debugging(&format!("Successfully refreshed marketplace: {name}"));Ok(())
    }.await;
    refreshed.map_err(|error| {
        crate::utils::debug::log_for_debugging_with_level(
            &format!("Failed to refresh marketplace {name}: {error}"),
            crate::utils::debug::DebugLogLevel::Error,
        );
        anyhow::anyhow!("Failed to refresh marketplace '{name}': {error}")
    })
}
/// Maps to: CC `utils/plugins/marketplaceManager.ts:2587-2639#setMarketplaceAutoUpdate`.
pub async fn set_marketplace_auto_update(name: &str, auto_update: bool) -> anyhow::Result<()> {
    let mut config = load_known_marketplaces_config().await?;
    let entry = config.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "Marketplace '{name}' not found. Available marketplaces: {}",
            crate::utils::process_env::ecmascript_object_entries(&config)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    if let Some(seed) = seed_dir_for(entry["installLocation"].as_str().unwrap_or("")) {
        anyhow::bail!(
            "Marketplace '{name}' is seed-managed ({}) and auto-update is always disabled for seed content. To update: ask your admin to update the seed.",
            seed.display()
        );
    }
    if entry["autoUpdate"].as_bool() == Some(auto_update) {
        return Ok(());
    }
    config.get_mut(name).unwrap()["autoUpdate"] = auto_update.into();
    save_known_marketplaces_config(config).await?;
    if let Some(source) = get_marketplace_declaring_source(name) {
        if let Some(declared) = crate::utils::settings::get_settings_for_source(source)
            .and_then(|s| s.extra_known_marketplaces)
            .and_then(|v| v.get(name).cloned())
        {
            save_marketplace_to_settings(
                name,
                &serde_json::json!({"source":declared["source"],"autoUpdate":auto_update}),
                source,
            )?;
        }
    }
    crate::utils::debug::log_for_debugging(&format!(
        "Set autoUpdate={auto_update} for marketplace: {name}"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    tokio::task_local! {
        // Native I/O fixture seam only: provide the two byte strings read from
        // the same config pathname, as the official fs.readFile oracle does.
        pub(super) static CONFIG_READS: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>;
    }

    #[tokio::test]
    async fn resolves_plugin_metadata_from_marketplace_cache_only() {
        let root = std::env::temp_dir().join(format!(
            "cometix-hint-marketplace-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let marketplace = root.join("market");
        std::fs::create_dir_all(marketplace.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            root.join("plugins/known_marketplaces.json"),
            serde_json::json!({"anthropic": {"installLocation": marketplace}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            marketplace.join(".claude-plugin/marketplace.json"),
            serde_json::json!({
                "name": "anthropic",
                "owner": {"name": "Anthropic"},
                "plugins": [{"name": "mail", "description": "Mail tools", "source": "./mail"}]
            })
            .to_string(),
        )
        .unwrap();

        let plugin = get_plugin_by_id_cache_only("mail@anthropic").await.unwrap();
        assert_eq!(plugin.entry["name"], "mail");
        assert_eq!(plugin.entry["description"], "Mail tools");
        assert!(
            get_plugin_by_id_cache_only("missing@anthropic")
                .await
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn marketplace_config_reader_matches_official_bun_schema_and_failure_branches() {
        use crate::utils::errors::ConfigParseError;
        let root =
            std::env::temp_dir().join(format!("marketplace-reader-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", "");
        let _cowork = EnvVarGuard::set("CLAUDE_CODE_USE_COWORK_PLUGINS", "0");
        // Verbatim Bun oracle outputs for marketplaceManager.ts:264-299 and
        // schemas.ts:1593-1629. Native JSON syntax diagnostics are a recorded
        // shared-parser boundary; schema messages/error identity are compared only
        // for this finite, representable corpus (nonfinite/UTF-16 remain partial).
        let rows: serde_json::Value = serde_json::from_str(r###"[{"name":"missing","data":{},"names":[],"logs":[]},{"name":"empty","input":"{}","data":{},"names":[],"logs":[]},{"name":"valid","input":"{\"2\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\",\"autoUpdate\":false},\"10\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"},\"z\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\",\"extra\":9},\"a\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"}}","data":{"2":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date","autoUpdate":false},"10":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date"},"z":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date"},"a":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date"}},"names":["2","10","z","a"],"logs":[]},{"name":"null","input":"null","error":{"message":"Marketplace configuration file is corrupted: : Invalid input: expected record, received null","config":true,"defaultConfig":null,"filePath":"$ROOT/null/known_marketplaces.json"},"logs":[["Marketplace configuration file is corrupted: : Invalid input: expected record, received null",{"level":"error"}]]},{"name":"array","input":"[]","error":{"message":"Marketplace configuration file is corrupted: : Invalid input: expected record, received array","config":true,"defaultConfig":[],"filePath":"$ROOT/array/known_marketplaces.json"},"logs":[["Marketplace configuration file is corrupted: : Invalid input: expected record, received array",{"level":"error"}]]},{"name":"incomplete","input":"{\"m\":{\"installLocation\":\"/cache\"}}","error":{"message":"Marketplace configuration file is corrupted: m.source: Invalid input: expected object, received undefined, m.lastUpdated: Invalid input: expected string, received undefined","config":true,"defaultConfig":{"m":{"installLocation":"/cache"}},"filePath":"$ROOT/incomplete/known_marketplaces.json"},"logs":[["Marketplace configuration file is corrupted: m.source: Invalid input: expected object, received undefined, m.lastUpdated: Invalid input: expected string, received undefined",{"level":"error"}]]},{"name":"badbool","input":"{\"m\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\",\"autoUpdate\":null}}","error":{"message":"Marketplace configuration file is corrupted: m.autoUpdate: Invalid input: expected boolean, received null","config":true,"defaultConfig":{"m":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date","autoUpdate":null}},"filePath":"$ROOT/badbool/known_marketplaces.json"},"logs":[["Marketplace configuration file is corrupted: m.autoUpdate: Invalid input: expected boolean, received null",{"level":"error"}]]},{"name":"badsource","input":"{\"m\":{\"source\":{\"source\":\"bad\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"}}","error":{"message":"Marketplace configuration file is corrupted: m.source.source: Invalid input","config":true,"defaultConfig":{"m":{"source":{"source":"bad"},"installLocation":"/cache","lastUpdated":"not-a-date"}},"filePath":"$ROOT/badsource/known_marketplaces.json"},"logs":[["Marketplace configuration file is corrupted: m.source.source: Invalid input",{"level":"error"}]]},{"name":"json","input":"{","error":{"message":"Failed to load marketplace configuration: JSON Parse error: Expected '}'","config":false},"logs":[["Failed to load marketplace configuration: JSON Parse error: Expected '}'",{"level":"error"}]]},{"name":"bom","input":"﻿{}","error":{"message":"Failed to load marketplace configuration: JSON Parse error: Unrecognized token '﻿'","config":false},"logs":[["Failed to load marketplace configuration: JSON Parse error: Unrecognized token '﻿'",{"level":"error"}]]},{"name":"duplicate","input":"{\"m\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"},\"m\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"last\",\"lastUpdated\":\"not-a-date\"}}","data":{"m":{"source":{"source":"directory","path":"/cache"},"installLocation":"last","lastUpdated":"not-a-date"}},"names":["m"],"logs":[]},{"name":"proto","input":"{\"m\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"},\"__proto__\":{\"source\":{\"source\":\"directory\",\"path\":\"/cache\"},\"installLocation\":\"/cache\",\"lastUpdated\":\"not-a-date\"}}","data":{"m":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date"}},"names":["m"],"logs":[]},{"name":"utf8","input":[123,34,255,34,58,123,34,115,111,117,114,99,101,34,58,123,34,115,111,117,114,99,101,34,58,34,100,105,114,101,99,116,111,114,121,34,44,34,112,97,116,104,34,58,34,47,99,97,99,104,101,34,125,44,34,105,110,115,116,97,108,108,76,111,99,97,116,105,111,110,34,58,34,47,99,97,99,104,101,34,44,34,108,97,115,116,85,112,100,97,116,101,100,34,58,34,110,111,116,45,97,45,100,97,116,101,34,125,125],"data":{"�":{"source":{"source":"directory","path":"/cache"},"installLocation":"/cache","lastUpdated":"not-a-date"}},"names":["�"],"logs":[]}]"###).unwrap();
        let file = get_known_marketplaces_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        for case in rows.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let bytes = case.get("input").map(|raw| {
                if let Some(bytes) = raw.as_array() {
                    bytes
                        .iter()
                        .map(|x| x.as_u64().unwrap() as u8)
                        .collect::<Vec<_>>()
                } else {
                    raw.as_str().unwrap().as_bytes().to_vec()
                }
            });
            if let Some(bytes) = &bytes {
                std::fs::write(&file, bytes).unwrap();
            } else {
                let _ = std::fs::remove_file(&file);
            }
            let result = load_known_marketplaces_config().await;
            if let Some(expected) = case.get("data") {
                let actual = result.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(
                    serde_json::Value::Object(actual.clone()),
                    *expected,
                    "{name}"
                );
                assert_eq!(
                    serde_json::json!(actual.keys().collect::<Vec<_>>()),
                    case["names"],
                    "{name}"
                );
            } else {
                let error = result.unwrap_err();
                let expected = &case["error"];
                if expected["config"] == true {
                    let error = error
                        .downcast_ref::<ConfigParseError>()
                        .expect("original ConfigParseError must not be wrapped as generic Error");
                    assert_eq!(
                        error.message,
                        expected["message"].as_str().unwrap(),
                        "{name}"
                    );
                    assert_eq!(error.file_path, file.to_string_lossy(), "{name}");
                    assert_eq!(error.default_config, expected["defaultConfig"], "{name}");
                } else {
                    assert!(
                        error
                            .to_string()
                            .starts_with("Failed to load marketplace configuration: "),
                        "{name}: {error}"
                    );
                    assert!(!error.is::<ConfigParseError>(), "{name}");
                }
            }
            assert_eq!(
                std::fs::read(&file).ok(),
                bytes,
                "reader must neither repair nor write: {name}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn marketplace_config_reader_matches_official_fresh_paths_and_io_failure() {
        let first =
            std::env::temp_dir().join(format!("marketplace-reader-first-{}", uuid::Uuid::new_v4()));
        let second = std::env::temp_dir().join(format!(
            "marketplace-reader-second-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &first);
        let file = get_known_marketplaces_file();
        std::fs::create_dir(&file).unwrap();
        // marketplaceManager.ts:284-298: only ENOENT is empty; EISDIR is Error.
        let error = load_known_marketplaces_config().await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Failed to load marketplace configuration: EISDIR: illegal operation on a directory, read"
        );
        assert!(!error.is::<crate::utils::errors::ConfigParseError>());
        assert!(file.is_dir());
        // :265-266 resolves the path on every call; no memo or seed overlay.
        let _second = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &second);
        assert!(load_known_marketplaces_config().await.unwrap().is_empty());
        assert!(!get_known_marketplaces_file().exists());
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
    #[tokio::test]
    async fn marketplace_config_path_matches_official_lexical_join_without_realpath() {
        let root = std::env::temp_dir().join(format!("marketplace-path-{}", uuid::Uuid::new_v4()));
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let value = serde_json::json!({"m": {"source": {"source":"directory","path":""}, "installLocation":"", "lastUpdated":""}});
        std::fs::write(cache.join("known_marketplaces.json"), value.to_string()).unwrap();
        let _cache = EnvVarGuard::set(
            "CLAUDE_CODE_PLUGIN_CACHE_DIR",
            root.join("missing/../cache"),
        );
        // marketplaceManager.ts:103 join normalizes even nonexistent components.
        assert_eq!(
            get_known_marketplaces_file(),
            cache.join("known_marketplaces.json")
        );
        assert_eq!(
            serde_json::Value::Object(load_known_marketplaces_config().await.unwrap()),
            value
        );
        assert!(!root.join("missing").exists());
        // Node join preserves leading relative parents and decomposed Unicode.
        for (input, expected) in [
            ("../../a/../b", "../../b/known_marketplaces.json"),
            ("/../../cache", "/cache/known_marketplaces.json"),
            (
                "./cafe\u{301}/../cafe\u{301}",
                "cafe\u{301}/known_marketplaces.json",
            ),
        ] {
            let _case = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", input);
            assert_eq!(get_known_marketplaces_file(), PathBuf::from(expected));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn marketplace_cache_only_matches_official_schema_raw_config_and_seed_boundaries() {
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("marketplace-cache-{}", uuid::Uuid::new_v4()));
        let primary = root.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &primary);
        let _seed = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_SEED_DIR", root.join("seed"));
        let config = get_known_marketplaces_file();
        let catalog = root.join("catalog.json");
        let manifest = json!({"name":"m","owner":{"name":"Owner"},"plugins":[
            {"name":"p","source":"./plugin","extra":"strip"}
        ],"extra":"strip"});
        std::fs::write(&catalog, manifest.to_string()).unwrap();
        let raw = json!({"m":{"installLocation":catalog}}).to_string();
        std::fs::write(&config, &raw).unwrap();
        // CC 2088-2096/2200-2210: raw config casts intentionally differ from
        // Safe; actual Bun oracle: raw-target-invalid-known-schema.
        assert!(load_known_marketplaces_config_safe().await.is_empty());
        let plugin = get_plugin_by_id_cache_only("p@m@ignored").await.unwrap();
        assert_eq!(
            plugin.entry,
            json!({"name":"p","source":"./plugin","strict":true})
        );
        assert_eq!(plugin.marketplace_install_location, Some(json!(catalog)));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), raw);
        let marketplace = get_marketplace_cache_only("m").await.unwrap();
        assert_eq!(
            marketplace,
            json!({"name":"m","owner":{"name":"Owner"},"plugins":[plugin.entry]})
        );
        // CC PluginMarketplaceSchema validates the entire catalog, not only
        // the requested plugin. One invalid sibling rejects every lookup.
        for invalid in [
            json!({"plugins":[{"name":"p","source":"./plugin"}]}),
            json!({"name":"m","owner":{"name":"Owner"},"plugins":[
                {"name":"p","source":"./plugin"},{"name":"bad","source":42}
            ]}),
        ] {
            std::fs::write(&catalog, invalid.to_string()).unwrap();
            assert!(get_plugin_by_id_cache_only("p@m").await.is_none());
            assert_eq!(
                std::fs::read_to_string(&catalog).unwrap(),
                invalid.to_string()
            );
        }
        std::fs::write(&catalog, manifest.to_string()).unwrap();
        // Raw JavaScript property access also supports array numeric keys.
        std::fs::write(&config, json!([{"installLocation":catalog}]).to_string()).unwrap();
        assert!(get_plugin_by_id_cache_only("p@0").await.is_some());
        for invalid in ["{", "null", "[]"] {
            std::fs::write(&config, invalid).unwrap();
            assert!(get_marketplace_cache_only("m").await.is_none());
            assert!(load_known_marketplaces_config_safe().await.is_empty());
            assert_eq!(std::fs::read_to_string(&config).unwrap(), invalid);
        }
        // CC cache-only neither discovers nor registers seeds. A seed only
        // becomes readable after a separate registration wrote the primary.
        let seed = root.join("seed");
        std::fs::create_dir_all(seed.join("marketplaces")).unwrap();
        std::fs::write(seed.join("marketplaces/m.json"), manifest.to_string()).unwrap();
        std::fs::write(
            seed.join("known_marketplaces.json"),
            json!({"m":{
                "source":{"source":"directory","path":"build-time"},
                "installLocation":"build-time","lastUpdated":"not-a-date"
            }})
            .to_string(),
        )
        .unwrap();
        std::fs::write(&config, "{}").unwrap();
        assert!(get_plugin_by_id_cache_only("p@m").await.is_none());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{}");
        std::fs::write(
            &config,
            json!({"m":{
                "source":{"source":"directory","path":"build-time"},
                "installLocation":seed.join("marketplaces/m.json"),
                "lastUpdated":"not-a-date","autoUpdate":false
            }})
            .to_string(),
        )
        .unwrap();
        assert!(get_plugin_by_id_cache_only("p@m").await.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn marketplace_cached_reader_matches_official_error_identity_and_fallback() {
        use crate::utils::errors::{ConfigParseError, get_errno_code};
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("marketplace-read-{}", uuid::Uuid::new_v4()));
        let nested = root.join(".claude-plugin/marketplace.json");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        // CC 1372-1405 and 2058-2074: ConfigParseError must retain the failing
        // nested pathname/defaultConfig and must never fall back to root.
        std::fs::write(&nested, "{").unwrap();
        let error = read_cached_marketplace(&root).await.unwrap_err();
        let error = error.downcast_ref::<ConfigParseError>().unwrap();
        assert!(
            error
                .message
                .starts_with(&format!("Invalid JSON in {}: ", nested.display()))
        );
        assert_eq!(error.file_path, nested.to_string_lossy());
        assert_eq!(error.default_config, json!("{"));
        std::fs::write(&nested, r#"{"plugins":[]}"#).unwrap();
        let error = read_cached_marketplace(&root).await.unwrap_err();
        let error = error.downcast_ref::<ConfigParseError>().unwrap();
        assert_eq!(
            error.message,
            format!(
                "Invalid schema: {} name: Invalid input: expected string, received undefined, owner: Invalid input: expected object, received undefined",
                nested.display()
            )
        );
        assert_eq!(error.default_config, json!({"plugins":[]}));
        // File installLocation produces ENOTDIR on the nested path, then reads
        // the original file. Missing paths preserve the fallback's ENOENT.
        let manifest = json!({"name":"m","owner":{"name":"Owner"},"plugins":[]});
        std::fs::write(&nested, manifest.to_string()).unwrap();
        assert_eq!(read_cached_marketplace(&nested).await.unwrap(), manifest);
        assert_eq!(read_cached_marketplace(&root).await.unwrap(), manifest);
        let missing = root.join("missing");
        let error = read_cached_marketplace(&missing).await.unwrap_err();
        assert_eq!(get_errno_code(&error), Some("ENOENT"));
        assert!(
            error
                .to_string()
                .ends_with(&format!("'{}'", missing.display()))
        );
        // Node join lexically removes missing/.. before opening nested content.
        assert_eq!(
            read_cached_marketplace(&root.join("missing/.."))
                .await
                .unwrap(),
            manifest
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marketplace_cached_reader_matches_official_nonfallback_io_errors() {
        use crate::utils::errors::get_errno_code;
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("marketplace-errno-{}", uuid::Uuid::new_v4()));
        let nested = root.join(".claude-plugin/marketplace.json");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        // Official injected-fs oracle confirms ELOOP and EACCES stop after the
        // nested read. A broad fallback here instead returns root's EISDIR.
        symlink("marketplace.json", &nested).unwrap();
        let error = read_cached_marketplace(&root).await.unwrap_err();
        assert_eq!(get_errno_code(&error), Some("ELOOP"));
        std::fs::remove_file(&nested).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            std::fs::write(&nested, "{}").unwrap();
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o000)).unwrap();
            let error = read_cached_marketplace(&root).await.unwrap_err();
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(get_errno_code(&error), Some("EACCES"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn marketplace_cache_lookup_matches_official_two_reads_and_unchecked_return() {
        use serde_json::json;
        let root = std::env::temp_dir().join(format!("marketplace-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _cache = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_CACHE_DIR", &root);
        std::fs::write(get_known_marketplaces_file(), "{}").unwrap();
        let catalog = root.join("catalog.json");
        std::fs::write(
            &catalog,
            json!({"name":"m","owner":{"name":"Owner"},
            "plugins":[{"name":"p","source":"./p"}]})
            .to_string(),
        )
        .unwrap();
        // The actual Bun fs.readFile oracle returns every first-read value,
        // including undefined, even though only the second-read value is a path.
        for location in [
            None,
            Some(json!(null)),
            Some(json!(42)),
            Some(json!({"x":1})),
            Some(json!("first-location")),
        ] {
            let mut first_entry = serde_json::Map::new();
            if let Some(location) = &location {
                first_entry.insert("installLocation".into(), location.clone());
            }
            let first = json!({"m":first_entry}).to_string().into_bytes();
            let second = json!({"m":{"installLocation":catalog}})
                .to_string()
                .into_bytes();
            CONFIG_READS
                .scope(std::cell::RefCell::new([first, second].into()), async {
                    let result = get_plugin_by_id_cache_only("p@m").await.unwrap();
                    assert_eq!(result.marketplace_install_location, location);
                    assert_eq!(
                        result.entry,
                        json!({"name":"p","source":"./p","strict":true})
                    );
                    CONFIG_READS.with(|reads| {
                        assert!(reads.borrow().is_empty(), "both source reads must execute")
                    });
                })
                .await;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_timeout_matches_source_parse_int_prefix_and_number_domain() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for (raw, expected) in [
            ("", 120000.0),
            ("0", 120000.0),
            ("-1", 120000.0),
            ("0x100", 120000.0),
            ("1e6", 1.0),
            ("+2500ms", 2500.0),
            ("\u{feff} 1499.9", 1499.0),
            ("\u{85}50", 120000.0),
            ("9007199254740993", 9007199254740992.0),
        ] {
            let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS", raw);
            assert_eq!(get_plugin_git_timeout_ms(), expected, "{raw:?}");
        }
        let large = "9".repeat(400);
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS", &large);
        assert_eq!(get_plugin_git_timeout_ms(), f64::INFINITY);
    }

    #[test]
    fn git_pull_error_precedence_preserves_returned_fields() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        use crate::utils::exec_file_no_throw::ExecFileOutput;
        let _env = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS", "1499");
        for (code, stderr, error, prefix) in [
            (
                0,
                "Host key verification failed",
                Some("timed out"),
                "Host key verification failed",
            ),
            (
                1,
                "Host key verification failed",
                Some("timed out"),
                "Git pull timed out after 1s.",
            ),
            (
                1,
                "REMOTE HOST IDENTIFICATION HAS CHANGED Host key verification failed Could not read from remote repository",
                None,
                "SSH host key for this marketplace's git host has changed",
            ),
            (
                1,
                "Host key verification failed Could not read from remote repository",
                None,
                "SSH host key verification failed while updating marketplace.",
            ),
            (
                1,
                "Permission denied (publickey) timed out",
                None,
                "SSH authentication failed while updating marketplace.",
            ),
            (
                1,
                "Could not read from remote repository",
                None,
                "SSH authentication failed while updating marketplace.",
            ),
            (
                1,
                "Could not resolve host",
                None,
                "Network error while updating marketplace.",
            ),
            (1, "timeout", None, "timeout"),
            (1, "Authentication failed", None, "Authentication failed"),
        ] {
            let result = enhance_git_pull_error_messages(ExecFileOutput {
                code,
                stdout: "unchanged".into(),
                stderr: stderr.into(),
                error: error.map(str::to_owned),
            });
            assert_eq!(result.code, code);
            assert_eq!(result.stdout, "unchanged");
            assert_eq!(result.error.as_deref(), error);
            assert!(result.stderr.starts_with(prefix), "{}", result.stderr);
            if prefix != stderr {
                assert!(
                    result
                        .stderr
                        .ends_with(&format!("\n\nOriginal error: {stderr}"))
                );
            }
        }
    }

    #[test]
    fn git_credentials_and_ssh_host_match_source_domains() {
        for (input, expected) in [
            (
                "https://user:token@github.com/repo",
                "https://***:***@github.com/repo",
            ),
            (
                "https://:token@github.com/repo",
                "https://:***@github.com/repo",
            ),
            (
                "https://token@github.com/repo",
                "https://***@github.com/repo",
            ),
            ("HTTPS://token@GitHub.COM:443", "https://***@github.com/"),
            (
                "https://user:@github.com/repo",
                "https://***@github.com/repo",
            ),
            ("https://GitHub.COM:443", "https://GitHub.COM:443"),
            (
                "ssh://git:token@github.com/repo",
                "ssh://git:token@github.com/repo",
            ),
            ("git@github.com:owner/repo", "git@github.com:owner/repo"),
            ("owner/repo", "owner/repo"),
        ] {
            assert_eq!(redact_url_credentials(input), expected, "{input}");
        }
        for (input, expected) in [
            ("git@github.com:owner/repo", Some("github.com")),
            ("ssh://git@host:22/repo", Some("host")),
            ("a@b@c:repo", Some("b@c")),
            ("git@[::1]:repo", Some("[")),
            ("git@:repo", None),
            ("https://host/repo", None),
        ] {
            assert_eq!(extract_ssh_host(input), expected, "{input}");
        }
        assert!(is_authentication_error("x403x"));
        assert!(is_authentication_error("terminal prompts disabled"));
        assert!(!is_authentication_error("authentication failed"));
        assert!(!is_authentication_error("Permission denied (publickey)"));
    }

    #[test]
    fn marketplace_progress_errors_do_not_abort_following_progress() {
        let messages = std::sync::Mutex::new(Vec::new());
        let callback = |message: &str| -> anyhow::Result<()> {
            messages.lock().unwrap().push(message.to_owned());
            anyhow::bail!("fixture callback failure")
        };
        safe_call_progress(None, "absent");
        safe_call_progress(Some(&callback), "first");
        safe_call_progress(Some(&callback), "second");
        assert_eq!(*messages.lock().unwrap(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn missing_local_marketplace_git_cache_fails_and_cleans_without_network() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-market-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cache = root.join("cache");
        let source = root.join("missing-local-source");
        let messages = std::sync::Mutex::new(Vec::new());
        let callback = |message: &str| -> anyhow::Result<()> {
            messages.lock().unwrap().push(message.to_owned());
            Ok(())
        };
        let error = cache_marketplace_from_git(
            &source.to_string_lossy(),
            &cache,
            None,
            None,
            Some(&callback),
            false,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Failed to clone marketplace repository:")
        );
        assert!(!cache.exists());
        assert_eq!(messages.lock().unwrap().len(), 2);
        assert!(messages.lock().unwrap()[0].starts_with("Refreshing marketplace cache (timeout:"));
        assert!(messages.lock().unwrap()[1].starts_with("Cloning repository (timeout:"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn local_git_marketplace_cache_matches_official_full_pull_sparse_transitions() {
        // Original TS/native Git oracle: plugin-marketplace-git-0914/native-git-oracle.json.
        // All remotes are local file:// paths; isolated Git config, no SSH/network.
        use crate::utils::exec_file_no_throw::{
            ExecFileStdin, ExecFileWithCwdOptions, exec_file_no_throw_with_cwd_options,
        };
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-market-transitions-{}",
            uuid::Uuid::new_v4()
        ));
        let source = root.join("source");
        let cache = root.join("cache");
        std::fs::create_dir_all(source.join("plugins/a")).unwrap();
        std::fs::create_dir_all(source.join("plugins/b")).unwrap();
        std::fs::write(source.join("plugins/a/value"), "a1").unwrap();
        std::fs::write(source.join("plugins/b/value"), "b1").unwrap();
        let config = root.join("gitconfig");
        std::fs::write(
            &config,
            "[user]\nname = Fixture\nemail = fixture@example.invalid\n[commit]\ngpgsign = false\n",
        )
        .unwrap();
        let _global = EnvVarGuard::set("GIT_CONFIG_GLOBAL", &config);
        let _system = EnvVarGuard::set("GIT_CONFIG_NOSYSTEM", "1");
        let _timeout = EnvVarGuard::set("CLAUDE_CODE_PLUGIN_GIT_TIMEOUT_MS", "10000");
        let _hooks = EnvVarGuard::unset("GIT_TEMPLATE_DIR");
        // Test-only fixture setup feeds the same existing runner, no new process loop.
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec!["commit", "-m", "initial"],
        ] {
            let result = exec_file_no_throw_with_cwd_options(
                &crate::utils::git::git_exe().to_string_lossy(),
                &args,
                ExecFileWithCwdOptions {
                    cwd: Some(&source),
                    stdin: ExecFileStdin::Ignore,
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(result.code, 0, "{}", result.stderr);
        }
        let source_url = url::Url::from_directory_path(&source).unwrap().to_string();
        let _protocol = EnvVarGuard::set("GIT_ALLOW_PROTOCOL", "file");
        // A self-source submodule pins the pre-submodule commit, so recursion
        // remains finite and every fetch stays on local disk.
        let added = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &["submodule", "add", &source_url, "modules/local"],
            ExecFileWithCwdOptions {
                cwd: Some(&source),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(added.code, 0, "{}", added.stderr);
        let committed = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &["commit", "-am", "add local submodule"],
            ExecFileWithCwdOptions {
                cwd: Some(&source),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(committed.code, 0, "{}", committed.stderr);

        cache_marketplace_from_git(&source_url, &cache, None, None, None, false)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.join("plugins/a/value")).unwrap(),
            "a1"
        );
        #[cfg(unix)]
        {
            let other = root.join("other/inner");
            std::fs::create_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(&other, root.join("alias")).unwrap();
            std::fs::remove_dir_all(cache.join("modules/local")).unwrap();
            let spelled_cwd = root.join("alias/../cache");
            // The raw OS path points at other/cache (absent); Node join and
            // the subprocess runtime both normalize it to root/cache.
            assert!(
                tokio::fs::metadata(spelled_cwd.join(".gitmodules"))
                    .await
                    .is_err()
            );
            git_submodule_update(&spelled_cwd, &[], &[], None).await;
            assert!(cache.join("modules/local/plugins/a/value").exists());
        }
        std::fs::write(source.join("plugins/a/value"), "a2").unwrap();
        let result = exec_file_no_throw_with_cwd_options(
            &crate::utils::git::git_exe().to_string_lossy(),
            &["commit", "-am", "update"],
            ExecFileWithCwdOptions {
                cwd: Some(&source),
                stdin: ExecFileStdin::Ignore,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.code, 0, "{}", result.stderr);
        cache_marketplace_from_git(&source_url, &cache, Some("main"), None, None, true)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.join("plugins/a/value")).unwrap(),
            "a2"
        );
        let sparse_a = vec!["plugins/a".to_owned()];
        cache_marketplace_from_git(&source_url, &cache, None, Some(&sparse_a), None, false)
            .await
            .unwrap();
        assert!(!cache.join("plugins/b/value").exists());
        let sparse_b = vec!["plugins/b".to_owned()];
        cache_marketplace_from_git(&source_url, &cache, None, Some(&sparse_b), None, false)
            .await
            .unwrap();
        assert!(!cache.join("plugins/a/value").exists());
        assert!(cache.join("plugins/b/value").exists());
        assert_eq!(reconcile_sparse_checkout(&cache, None).await.code, 1);
        cache_marketplace_from_git(&source_url, &cache, None, None, None, false)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.join("plugins/a/value")).unwrap(),
            "a2"
        );
        assert!(cache.join("plugins/b/value").exists());
        let sparse_cache = root.join("sparse-cache");
        assert_eq!(
            git_clone(&source_url, &sparse_cache, Some("main"), Some(&sparse_a))
                .await
                .code,
            0
        );
        assert!(sparse_cache.join("plugins/a/value").exists());
        assert!(!sparse_cache.join("plugins/b/value").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn marketplace_service_local_settings_memo_and_mutations_match_original() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("market-service-{}", uuid::Uuid::new_v4()));
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let name = format!("fixture-{}", uuid::Uuid::new_v4().simple());
        let source = serde_json::json!({"source":"settings","name":name,"plugins":[]});
        let added = add_marketplace_source(&source, None).await.unwrap();
        assert_eq!(added.name, name);
        assert!(!added.already_materialized);
        assert!(
            add_marketplace_source(&source, None)
                .await
                .unwrap()
                .already_materialized
        );
        let first = get_marketplace(&name);
        let second = get_marketplace(&name);
        let (first, second) = tokio::join!(first, second);
        assert!(std::sync::Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
        let before = load_known_marketplaces_config().await.unwrap();
        refresh_marketplace(&name, None, false).await.unwrap();
        assert_eq!(load_known_marketplaces_config().await.unwrap(), before);
        set_marketplace_auto_update(&name, true).await.unwrap();
        assert_eq!(
            load_known_marketplaces_config().await.unwrap()[&name]["autoUpdate"],
            true
        );
        let local = root.join("local");
        std::fs::create_dir_all(local.join(".claude-plugin")).unwrap();
        let local_name = format!("local-{}", uuid::Uuid::new_v4().simple());
        std::fs::write(
            local.join(".claude-plugin/marketplace.json"),
            serde_json::json!({"name":local_name,"owner":{"name":"fixture"},"plugins":[]})
                .to_string(),
        )
        .unwrap();
        add_marketplace_source(
            &serde_json::json!({"source":"directory","path":local}),
            None,
        )
        .await
        .unwrap();
        refresh_marketplace(&local_name, None, false).await.unwrap();
        refresh_all_marketplaces().await.unwrap();
        remove_marketplace_source(&local_name).await.unwrap();
        assert!(local.exists());
        remove_marketplace_source(&name).await.unwrap();
        assert!(load_known_marketplaces_config().await.unwrap().is_empty());
        clear_marketplaces_cache();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn marketplace_node_path_and_basename_match_original() {
        assert_eq!(
            marketplace_path!("/base", "/absolute", "..", "next"),
            PathBuf::from("/base/next")
        );
        assert_eq!(marketplace_path!("", ""), PathBuf::from("."));
        assert_eq!(marketplace_path!("a", "../../b"), PathBuf::from("../b"));
        assert_eq!(
            get_cache_path_for_source(
                &serde_json::json!({"source":"file","path":"/a/foo.json.json"})
            ),
            "foo.json"
        );
        assert_eq!(
            get_cache_path_for_source(&serde_json::json!({"source":"directory","path":"/a/."})),
            "."
        );
        assert_eq!(
            get_cache_path_for_source(&serde_json::json!({"source":"directory","path":"/a/.."})),
            ".."
        );
        assert_eq!(
            get_cache_path_for_source(&serde_json::json!({"source":"github","repo":"a/b/c"})),
            "a-b/c"
        );
    }

    #[tokio::test]
    async fn marketplace_url_cache_schema_status_and_headers_match_original() {
        // Same TLS provider as main/print; this test bypasses those entrypoints.
        crate::utils::tls_provider::install_crypto_provider();
        use std::io::{Read, Write};
        let root = std::env::temp_dir().join(format!("market-url-{}", uuid::Uuid::new_v4()));
        for (status, body, valid) in [
            (
                200,
                r#"{"name":"url-fixture","owner":{"name":"fixture"},"plugins":[]}"#,
                true,
            ),
            (404, "not found", false),
            (200, "not json", false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert_eq!(request.matches("user-agent:").count(), 1);
                assert!(request.contains("user-agent: claude-code-plugin-manager"));
                write!(stream,"HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let target = root.join(format!("{status}-{valid}.json"));
            let headers = serde_json::json!({"user-agent":"must-be-replaced","X-Fixture":"yes"});
            let result = cache_marketplace_from_url(
                &format!("http://{address}/manifest"),
                &target,
                headers.as_object(),
                None,
            )
            .await;
            server.join().unwrap();
            if valid {
                result.unwrap();
                assert_eq!(
                    parse_file_with_schema(
                        &target,
                        super::super::schemas::plugin_marketplace_schema()
                    )
                    .await
                    .unwrap()["name"],
                    "url-fixture"
                );
            } else if status == 404 {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .starts_with("HTTP 404 error while downloading marketplace")
                );
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .is::<crate::utils::errors::ConfigParseError>()
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
