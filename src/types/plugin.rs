//! Maps to: CC `types/plugin.ts` (error union and display helper).
//!
//! L1: the TypeScript discriminated union is a serde-tagged Rust enum; each
//! variant retains its own source-defined fields. Optional properties use
//! `Option` and are omitted when absent. No shared path/error envelope exists.

use crate::utils::zod::javascript_number_to_string;
use serde::{Deserialize, Serialize};

/// Native shared property carrier for LoadedPlugin.mcpServers/lspServers.
/// Maps to types/plugin.ts and the assignments in refresh.ts / extract*Servers.
/// The established shallow-identity contract (translation-contracts; MODULE_MAP
/// types/plugin.ts) requires clones of a cached plugin to observe later writes.
/// This does not deduplicate loads: callers snapshot, release the lock, perform
/// source I/O, then assign. In particular Some({}) is present and None is absent.
#[derive(Clone, Debug, Default)]
pub struct PluginServerCache(
    std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<serde_json::Value>>>>,
);

impl PluginServerCache {
    pub fn snapshot(&self) -> Option<serde_json::Value> {
        self.identity_snapshot().map(|value| (*value).clone())
    }
    /// A render dependency observes the current property's object, not the slot.
    /// Retaining this Arc also prevents pointer reuse while a prior render lives.
    pub fn identity_snapshot(&self) -> Option<std::sync::Arc<serde_json::Value>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn set(&self, value: Option<serde_json::Value>) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value.map(std::sync::Arc::new);
    }
    pub fn is_some(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
    pub fn is_none(&self) -> bool {
        !self.is_some()
    }
}
impl From<Option<serde_json::Value>> for PluginServerCache {
    fn from(value: Option<serde_json::Value>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            value.map(std::sync::Arc::new),
        )))
    }
}
impl PartialEq for PluginServerCache {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.snapshot() == other.snapshot()
    }
}
impl Eq for PluginServerCache {}

/// Native shared array for the memoized PluginLoadResult.errors. Source refresh
/// and useManagePlugins pass this same array into their loaders; unrelated
/// config consumers still create their own error arrays. Never retain a lock
/// during loader I/O or while publishing an AppState snapshot.
#[derive(Clone, Debug, Default)]
pub struct PluginLoadErrors(std::sync::Arc<std::sync::Mutex<Vec<PluginError>>>);
impl PluginLoadErrors {
    pub fn snapshot(&self) -> Vec<PluginError> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn as_mutex(&self) -> &std::sync::Mutex<Vec<PluginError>> {
        &self.0
    }
    pub fn push(&self, error: PluginError) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(error);
    }
    pub fn extend(&self, errors: impl IntoIterator<Item = PluginError>) {
        let errors: Vec<_> = errors.into_iter().collect();
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(errors);
    }
    pub fn len(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl From<Vec<PluginError>> for PluginLoadErrors {
    fn from(value: Vec<PluginError>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(value)))
    }
}
impl PartialEq for PluginLoadErrors {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.snapshot() == other.snapshot()
    }
}

/// Maps to: CC `types/plugin.ts:48-70#LoadedPlugin`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadedPlugin {
    /// Maps to CC `LoadedPlugin.name`.
    pub name: String,
    /// Maps to CC `LoadedPlugin.source` (`<name>@inline` for `--plugin-dir`).
    pub source: String,
    /// Maps to CC `LoadedPlugin.path`.
    pub path: std::path::PathBuf,
    /// Maps to CC `LoadedPlugin.manifest`.
    pub manifest: crate::utils::plugins::schemas::PluginManifest,
    /// Maps to CC `LoadedPlugin.agentsPath`.
    pub agents_path: Option<std::path::PathBuf>,
    /// Maps to CC `LoadedPlugin.agentsPaths`.
    pub agents_paths: Vec<std::path::PathBuf>,
    /// Maps to CC `LoadedPlugin.enabled`.
    pub enabled: bool,
    /// Maps to CC `LoadedPlugin.isBuiltin`.
    pub is_builtin: bool,
    /// Maps to CC `LoadedPlugin.hooksConfig`; carried for the shared plugin
    /// load-result shape, not executed by the agent-loader slice.
    pub hooks_config: Option<serde_json::Value>,
    /// Maps to CC `LoadedPlugin.mcpServers`; carried for shared plugin source
    /// parity, not connected from the agent-loader slice.
    pub mcp_servers: PluginServerCache,
    /// Maps to CC `LoadedPlugin.lspServers`; carried for shared plugin source
    /// parity and cache-only LSP activation.
    pub lsp_servers: PluginServerCache,
    pub repository: String,
    pub sha: Option<String>,
    pub commands_path: Option<std::path::PathBuf>,
    pub commands_paths: Vec<std::path::PathBuf>,
    pub commands_metadata: Option<serde_json::Value>,
    pub skills_path: Option<std::path::PathBuf>,
    pub skills_paths: Vec<std::path::PathBuf>,
    pub output_styles_path: Option<std::path::PathBuf>,
    pub output_styles_paths: Vec<std::path::PathBuf>,
    pub settings: Option<serde_json::Value>,
}

/// Maps to: CC `types/plugin.ts#PluginLoadResult`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginLoadResult {
    pub enabled: Vec<LoadedPlugin>,
    pub disabled: Vec<LoadedPlugin>,
    pub errors: PluginLoadErrors,
}

/// Maps to: CC `types/plugin.ts#PluginComponent:72-77`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginComponent {
    Commands,
    Agents,
    Skills,
    Hooks,
    OutputStyles,
}

impl PluginComponent {
    /// L1: string-literal representation used by the source template strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Hooks => "hooks",
            Self::OutputStyles => "output-styles",
        }
    }
}

/// Maps to: CC `types/plugin.ts#PluginError.authType:114` (inline literal union).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginGitAuthType {
    Ssh,
    Https,
}

/// Maps to: CC `types/plugin.ts#PluginError.operation:121` (inline literal union).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginGitOperation {
    Clone,
    Pull,
}

/// Maps to: CC `types/plugin.ts#PluginError.reason:270` (inline literal union).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginDependencyReason {
    NotEnabled,
    NotFound,
}

/// Maps to: CC `types/plugin.ts#PluginError:101-283`.
/// The source repeats the identical `lsp-config-invalid` union member twice;
/// one Rust variant represents that same discriminant and field set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum PluginError {
    PathNotFound {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        path: String,
        component: PluginComponent,
    },
    GitAuthFailed {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        git_url: String,
        auth_type: PluginGitAuthType,
    },
    GitTimeout {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        git_url: String,
        operation: PluginGitOperation,
    },
    NetworkError {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
    ManifestParseError {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        manifest_path: String,
        parse_error: String,
    },
    ManifestValidationError {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        manifest_path: String,
        validation_errors: Vec<String>,
    },
    PluginNotFound {
        source: String,
        plugin_id: String,
        marketplace: String,
    },
    MarketplaceNotFound {
        source: String,
        marketplace: String,
        available_marketplaces: Vec<String>,
    },
    MarketplaceLoadFailed {
        source: String,
        marketplace: String,
        reason: String,
    },
    McpConfigInvalid {
        source: String,
        plugin: String,
        server_name: String,
        validation_error: String,
    },
    McpServerSuppressedDuplicate {
        source: String,
        plugin: String,
        server_name: String,
        duplicate_of: String,
    },
    LspConfigInvalid {
        source: String,
        plugin: String,
        server_name: String,
        validation_error: String,
    },
    HookLoadFailed {
        source: String,
        plugin: String,
        hook_path: String,
        reason: String,
    },
    ComponentLoadFailed {
        source: String,
        plugin: String,
        component: PluginComponent,
        path: String,
        reason: String,
    },
    McpbDownloadFailed {
        source: String,
        plugin: String,
        url: String,
        reason: String,
    },
    McpbExtractFailed {
        source: String,
        plugin: String,
        mcpb_path: String,
        reason: String,
    },
    McpbInvalidManifest {
        source: String,
        plugin: String,
        mcpb_path: String,
        validation_error: String,
    },
    LspServerStartFailed {
        source: String,
        plugin: String,
        server_name: String,
        reason: String,
    },
    LspServerCrashed {
        source: String,
        plugin: String,
        server_name: String,
        exit_code: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    LspRequestTimeout {
        source: String,
        plugin: String,
        server_name: String,
        method: String,
        timeout_ms: f64,
    },
    LspRequestFailed {
        source: String,
        plugin: String,
        server_name: String,
        method: String,
        error: String,
    },
    MarketplaceBlockedByPolicy {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        marketplace: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_by_blocklist: Option<bool>,
        allowed_sources: Vec<String>,
    },
    DependencyUnsatisfied {
        source: String,
        plugin: String,
        dependency: String,
        reason: PluginDependencyReason,
    },
    PluginCacheMiss {
        source: String,
        plugin: String,
        // Raw marketplace cache lookup can observe undefined/null/other values
        // across its two reads (marketplaceManager.ts:2188-2223); the local
        // stat catch passes the value through despite the TS string annotation.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_cache_miss_location"
        )]
        install_path: Option<serde_json::Value>,
    },
    GenericError {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        error: String,
    },
}

impl PluginError {
    /// L1: field-only projection of the common source property on every member.
    pub fn source(&self) -> &str {
        match self {
            Self::PathNotFound { source, .. }
            | Self::GitAuthFailed { source, .. }
            | Self::GitTimeout { source, .. }
            | Self::NetworkError { source, .. }
            | Self::ManifestParseError { source, .. }
            | Self::ManifestValidationError { source, .. }
            | Self::PluginNotFound { source, .. }
            | Self::MarketplaceNotFound { source, .. }
            | Self::MarketplaceLoadFailed { source, .. }
            | Self::McpConfigInvalid { source, .. }
            | Self::McpServerSuppressedDuplicate { source, .. }
            | Self::LspConfigInvalid { source, .. }
            | Self::HookLoadFailed { source, .. }
            | Self::ComponentLoadFailed { source, .. }
            | Self::McpbDownloadFailed { source, .. }
            | Self::McpbExtractFailed { source, .. }
            | Self::McpbInvalidManifest { source, .. }
            | Self::LspServerStartFailed { source, .. }
            | Self::LspServerCrashed { source, .. }
            | Self::LspRequestTimeout { source, .. }
            | Self::LspRequestFailed { source, .. }
            | Self::MarketplaceBlockedByPolicy { source, .. }
            | Self::DependencyUnsatisfied { source, .. }
            | Self::PluginCacheMiss { source, .. }
            | Self::GenericError { source, .. } => source,
        }
    }
}

/// Maps to: CC `types/plugin.ts#getPluginErrorMessage:295-363`.
pub fn get_plugin_error_message(error: &PluginError) -> String {
    match error {
        PluginError::GenericError { error, .. } => error.clone(),
        PluginError::PathNotFound {
            path, component, ..
        } => {
            format!("Path not found: {path} ({})", component.as_str())
        }
        PluginError::GitAuthFailed {
            git_url, auth_type, ..
        } => {
            let auth_type = match auth_type {
                PluginGitAuthType::Ssh => "ssh",
                PluginGitAuthType::Https => "https",
            };
            format!("Git authentication failed ({auth_type}): {git_url}")
        }
        PluginError::GitTimeout {
            git_url, operation, ..
        } => {
            let operation = match operation {
                PluginGitOperation::Clone => "clone",
                PluginGitOperation::Pull => "pull",
            };
            format!("Git {operation} timeout: {git_url}")
        }
        PluginError::NetworkError { url, details, .. } => {
            format!(
                "Network error: {url}{}",
                match details {
                    Some(details) if !details.is_empty() => format!(" - {details}"),
                    _ => String::new(),
                }
            )
        }
        PluginError::ManifestParseError { parse_error, .. } => {
            format!("Manifest parse error: {parse_error}")
        }
        PluginError::ManifestValidationError {
            validation_errors, ..
        } => {
            format!(
                "Manifest validation failed: {}",
                validation_errors.join(", ")
            )
        }
        PluginError::PluginNotFound {
            plugin_id,
            marketplace,
            ..
        } => {
            format!("Plugin {plugin_id} not found in marketplace {marketplace}")
        }
        PluginError::MarketplaceNotFound { marketplace, .. } => {
            format!("Marketplace {marketplace} not found")
        }
        PluginError::MarketplaceLoadFailed {
            marketplace,
            reason,
            ..
        } => {
            format!("Marketplace {marketplace} failed to load: {reason}")
        }
        PluginError::McpConfigInvalid {
            server_name,
            validation_error,
            ..
        } => {
            format!("MCP server {server_name} invalid: {validation_error}")
        }
        PluginError::McpServerSuppressedDuplicate {
            server_name,
            duplicate_of,
            ..
        } => {
            let dup = if duplicate_of.starts_with("plugin:") {
                format!(
                    "server provided by plugin \"{}\"",
                    duplicate_of.split(':').nth(1).unwrap_or("?")
                )
            } else {
                format!("already-configured \"{duplicate_of}\"")
            };
            format!("MCP server \"{server_name}\" skipped — same command/URL as {dup}")
        }
        PluginError::HookLoadFailed { reason, .. } => format!("Hook load failed: {reason}"),
        PluginError::ComponentLoadFailed {
            component,
            path,
            reason,
            ..
        } => {
            format!("{} load failed from {path}: {reason}", component.as_str())
        }
        PluginError::McpbDownloadFailed { url, reason, .. } => {
            format!("Failed to download MCPB from {url}: {reason}")
        }
        PluginError::McpbExtractFailed {
            mcpb_path, reason, ..
        } => {
            format!("Failed to extract MCPB {mcpb_path}: {reason}")
        }
        PluginError::McpbInvalidManifest {
            mcpb_path,
            validation_error,
            ..
        } => {
            format!("MCPB manifest invalid at {mcpb_path}: {validation_error}")
        }
        PluginError::LspConfigInvalid {
            plugin,
            server_name,
            validation_error,
            ..
        } => {
            format!(
                "Plugin \"{plugin}\" has invalid LSP server config for \"{server_name}\": {validation_error}"
            )
        }
        PluginError::LspServerStartFailed {
            plugin,
            server_name,
            reason,
            ..
        } => {
            format!("Plugin \"{plugin}\" failed to start LSP server \"{server_name}\": {reason}")
        }
        PluginError::LspServerCrashed {
            plugin,
            server_name,
            exit_code,
            signal,
            ..
        } => {
            if let Some(signal) = signal.as_ref().filter(|signal| !signal.is_empty()) {
                return format!(
                    "Plugin \"{plugin}\" LSP server \"{server_name}\" crashed with signal {signal}"
                );
            }
            let exit_code = exit_code
                .map(javascript_number_to_string)
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "Plugin \"{plugin}\" LSP server \"{server_name}\" crashed with exit code {exit_code}"
            )
        }
        PluginError::LspRequestTimeout {
            plugin,
            server_name,
            method,
            timeout_ms,
            ..
        } => {
            format!(
                "Plugin \"{plugin}\" LSP server \"{server_name}\" timed out on {method} request after {}ms",
                javascript_number_to_string(*timeout_ms)
            )
        }
        PluginError::LspRequestFailed {
            plugin,
            server_name,
            method,
            error,
            ..
        } => {
            format!(
                "Plugin \"{plugin}\" LSP server \"{server_name}\" {method} request failed: {error}"
            )
        }
        PluginError::MarketplaceBlockedByPolicy {
            marketplace,
            blocked_by_blocklist,
            ..
        } => {
            if blocked_by_blocklist.unwrap_or(false) {
                return format!("Marketplace '{marketplace}' is blocked by enterprise policy");
            }
            format!("Marketplace '{marketplace}' is not in the allowed marketplace list")
        }
        PluginError::DependencyUnsatisfied {
            dependency, reason, ..
        } => {
            let hint = match reason {
                PluginDependencyReason::NotEnabled => {
                    "disabled — enable it or remove the dependency"
                }
                PluginDependencyReason::NotFound => "not found in any configured marketplace",
            };
            format!("Dependency \"{dependency}\" is {hint}")
        }
        PluginError::PluginCacheMiss {
            plugin,
            install_path,
            ..
        } => {
            let install_path = install_path
                .as_ref()
                .map(crate::utils::zod::js_string)
                .unwrap_or_else(|| "undefined".into());
            format!("Plugin \"{plugin}\" not cached at {install_path} — run /plugins to refresh")
        }
    }
}

// L1 serde presence adapter for PluginCacheMiss.installPath; explicit null
// must survive as Some(Null), unlike serde's default Option deserializer.
fn deserialize_cache_miss_location<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cloned_plugin_load_result_matches_official_partial_refresh_visibility() {
        use super::*;
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/oracles/mcp-contract-review-0915/refresh-oracle.json"
        ))
        .unwrap();
        let loaded = PluginLoadResult {
            enabled: vec![LoadedPlugin::default(), LoadedPlugin::default()],
            errors: vec![PluginError::GenericError {
                source: "existing".into(),
                plugin: None,
                error: "existing".into(),
            }]
            .into(),
            ..Default::default()
        };
        let cached = loaded.clone();
        let worker_copy = loaded.clone();
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            worker_copy.enabled[1]
                .mcp_servers
                .set(Some(serde_json::json!({"first":{},"second":{}})));
            worker_copy.errors.push(PluginError::GenericError {
                source: "loading".into(),
                plugin: None,
                error: "loading".into(),
            });
            published_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            worker_copy.enabled[0]
                .lsp_servers
                .set(Some(serde_json::json!({})));
        });
        published_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // Source partial completion is visible before the Promise.all finishes.
        let snapshot = cached.enabled[1].mcp_servers.snapshot().unwrap();
        assert_eq!(
            serde_json::json!(snapshot.as_object().unwrap().keys().collect::<Vec<_>>()),
            oracle["earlyCache"]["p2McpKeys"]
        );
        assert_eq!(
            cached.enabled[0].mcp_servers.is_none(),
            oracle["earlyCache"]["p1McpAbsent"].as_bool().unwrap()
        );
        assert_eq!(
            cached.errors.len(),
            oracle["earlyCache"]["sharedErrorCount"].as_u64().unwrap() as usize
        );
        let app_errors_snapshot = cached.errors.snapshot();
        finish_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            loaded.enabled[0].lsp_servers.snapshot(),
            Some(serde_json::json!({}))
        );
        assert!(loaded.enabled[0].lsp_servers.is_some());
        let before = loaded.enabled[0].lsp_servers.identity_snapshot().unwrap();
        let same_property = cached.enabled[0].lsp_servers.identity_snapshot().unwrap();
        assert!(std::sync::Arc::ptr_eq(&before, &same_property));
        // Source assignment of a new value-equal object still changes useEffect deps.
        cached.enabled[0]
            .lsp_servers
            .set(Some(serde_json::json!({})));
        let after = loaded.enabled[0].lsp_servers.identity_snapshot().unwrap();
        assert!(!std::sync::Arc::ptr_eq(&before, &after));
        assert_eq!(*before, *after);
        cached.errors.push(PluginError::GenericError {
            source: "later".into(),
            plugin: None,
            error: "later".into(),
        });
        assert_eq!(
            app_errors_snapshot.len(),
            2,
            "source AppState merge creates a separate error array"
        );
        assert_eq!(loaded.errors.len(), 3);
        assert_eq!(
            loaded, cached,
            "alias equality must not deadlock by taking its mutex twice"
        );
        assert!(PluginLoadResult::default().errors.is_empty());
        assert!(
            LoadedPlugin::default().lsp_servers.is_none(),
            "new loads own fresh slots"
        );
    }
    use super::*;

    /// Actual Bun `getPluginErrorMessage` oracle; CC types/plugin.ts:295-363.
    /// All 25 unique union tags, five components and optional/truthiness edges.
    #[test]
    fn plugin_error_messages_and_fields_match_official_oracle() {
        #[rustfmt::skip]
        let cases: &[(&str, &str)] = &[
            (r#"{"source":"source","type":"generic-error","error":"failed to load manifest"}"#, "failed to load manifest"),
            (r#"{"source":"source","type":"generic-error","plugin":"","error":""}"#, ""),
            (r#"{"source":"source","type":"path-not-found","path":"/missing","component":"commands"}"#, "Path not found: /missing (commands)"),
            (r#"{"source":"source","type":"path-not-found","path":"/missing","component":"agents"}"#, "Path not found: /missing (agents)"),
            (r#"{"source":"source","type":"path-not-found","path":"/missing","component":"skills"}"#, "Path not found: /missing (skills)"),
            (r#"{"source":"source","type":"path-not-found","path":"/missing","component":"hooks"}"#, "Path not found: /missing (hooks)"),
            (r#"{"source":"source","type":"path-not-found","path":"/missing","component":"output-styles"}"#, "Path not found: /missing (output-styles)"),
            (r#"{"source":"source","type":"git-auth-failed","gitUrl":"git://repo","authType":"ssh"}"#, "Git authentication failed (ssh): git://repo"),
            (r#"{"source":"source","type":"git-auth-failed","gitUrl":"git://repo","authType":"https"}"#, "Git authentication failed (https): git://repo"),
            (r#"{"source":"source","type":"git-timeout","gitUrl":"git://repo","operation":"clone"}"#, "Git clone timeout: git://repo"),
            (r#"{"source":"source","type":"git-timeout","gitUrl":"git://repo","operation":"pull"}"#, "Git pull timeout: git://repo"),
            (r#"{"source":"source","type":"network-error","url":"https://example.test"}"#, "Network error: https://example.test"),
            (r#"{"source":"source","type":"network-error","url":"https://example.test","details":""}"#, "Network error: https://example.test"),
            (r#"{"source":"source","type":"network-error","url":"https://example.test","details":" "}"#, "Network error: https://example.test -  "),
            (r#"{"source":"source","type":"network-error","url":"https://example.test","details":"refused"}"#, "Network error: https://example.test - refused"),
            (r#"{"source":"source","type":"manifest-parse-error","manifestPath":"/manifest","parseError":"Unexpected token"}"#, "Manifest parse error: Unexpected token"),
            (r#"{"source":"source","type":"manifest-validation-error","manifestPath":"/manifest","validationErrors":[]}"#, "Manifest validation failed: "),
            (r#"{"source":"source","type":"manifest-validation-error","manifestPath":"/manifest","validationErrors":["first"]}"#, "Manifest validation failed: first"),
            (r#"{"source":"source","type":"manifest-validation-error","manifestPath":"/manifest","validationErrors":["first","second"]}"#, "Manifest validation failed: first, second"),
            (r#"{"source":"source","type":"manifest-validation-error","manifestPath":"/manifest","validationErrors":["",""]}"#, "Manifest validation failed: , "),
            (r#"{"source":"source","type":"plugin-not-found","pluginId":"name@market","marketplace":"market"}"#, "Plugin name@market not found in marketplace market"),
            (r#"{"source":"source","type":"marketplace-not-found","marketplace":"market","availableMarketplaces":["other"]}"#, "Marketplace market not found"),
            (r#"{"source":"source","type":"marketplace-load-failed","marketplace":"market","reason":"unreadable"}"#, "Marketplace market failed to load: unreadable"),
            (r#"{"source":"source","type":"mcp-config-invalid","plugin":"p","serverName":"s","validationError":"bad"}"#, "MCP server s invalid: bad"),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"plugin:p:s"}"#, "MCP server \"s\" skipped — same command/URL as server provided by plugin \"p\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"plugin:"}"#, "MCP server \"s\" skipped — same command/URL as server provided by plugin \"\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"plugin::s"}"#, "MCP server \"s\" skipped — same command/URL as server provided by plugin \"\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"plugin:p:more:parts"}"#, "MCP server \"s\" skipped — same command/URL as server provided by plugin \"p\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"plugin"}"#, "MCP server \"s\" skipped — same command/URL as already-configured \"plugin\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":""}"#, "MCP server \"s\" skipped — same command/URL as already-configured \"\""),
            (r#"{"source":"source","type":"mcp-server-suppressed-duplicate","plugin":"p","serverName":"s","duplicateOf":"user:s"}"#, "MCP server \"s\" skipped — same command/URL as already-configured \"user:s\""),
            (r#"{"source":"source","type":"lsp-config-invalid","plugin":"p","serverName":"s","validationError":"bad"}"#, "Plugin \"p\" has invalid LSP server config for \"s\": bad"),
            (r#"{"source":"source","type":"hook-load-failed","plugin":"p","hookPath":"/hook","reason":"bad"}"#, "Hook load failed: bad"),
            (r#"{"source":"source","type":"component-load-failed","plugin":"p","component":"output-styles","path":"/style","reason":"bad"}"#, "output-styles load failed from /style: bad"),
            (r#"{"source":"source","type":"mcpb-download-failed","plugin":"p","url":"https://example.test/file","reason":"bad"}"#, "Failed to download MCPB from https://example.test/file: bad"),
            (r#"{"source":"source","type":"mcpb-extract-failed","plugin":"p","mcpbPath":"/bundle","reason":"bad"}"#, "Failed to extract MCPB /bundle: bad"),
            (r#"{"source":"source","type":"mcpb-invalid-manifest","plugin":"p","mcpbPath":"/bundle","validationError":"bad"}"#, "MCPB manifest invalid at /bundle: bad"),
            (r#"{"source":"source","type":"lsp-server-start-failed","plugin":"p","serverName":"s","reason":"bad"}"#, "Plugin \"p\" failed to start LSP server \"s\": bad"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":null}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code unknown"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":null,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code unknown"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":null,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":0}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 0"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":0,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 0"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":0,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":-1}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code -1"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":-1,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code -1"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":-1,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1.5}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1.5"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1.5,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1.5"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1.5,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e+21}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1e+21"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e+21,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1e+21"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e+21,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e-07}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1e-7"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e-07,"signal":""}"#, "Plugin \"p\" LSP server \"s\" crashed with exit code 1e-7"),
            (r#"{"source":"source","type":"lsp-server-crashed","plugin":"p","serverName":"s","exitCode":1e-07,"signal":"SIGTERM"}"#, "Plugin \"p\" LSP server \"s\" crashed with signal SIGTERM"),
            (r#"{"source":"source","type":"lsp-request-timeout","plugin":"p","serverName":"s","method":"initialize","timeoutMs":0}"#, "Plugin \"p\" LSP server \"s\" timed out on initialize request after 0ms"),
            (r#"{"source":"source","type":"lsp-request-timeout","plugin":"p","serverName":"s","method":"initialize","timeoutMs":-1}"#, "Plugin \"p\" LSP server \"s\" timed out on initialize request after -1ms"),
            (r#"{"source":"source","type":"lsp-request-timeout","plugin":"p","serverName":"s","method":"initialize","timeoutMs":1.5}"#, "Plugin \"p\" LSP server \"s\" timed out on initialize request after 1.5ms"),
            (r#"{"source":"source","type":"lsp-request-timeout","plugin":"p","serverName":"s","method":"initialize","timeoutMs":1e+21}"#, "Plugin \"p\" LSP server \"s\" timed out on initialize request after 1e+21ms"),
            (r#"{"source":"source","type":"lsp-request-timeout","plugin":"p","serverName":"s","method":"initialize","timeoutMs":1e-07}"#, "Plugin \"p\" LSP server \"s\" timed out on initialize request after 1e-7ms"),
            (r#"{"source":"source","type":"lsp-request-failed","plugin":"p","serverName":"s","method":"initialize","error":"bad"}"#, "Plugin \"p\" LSP server \"s\" initialize request failed: bad"),
            (r#"{"source":"source","type":"marketplace-blocked-by-policy","marketplace":"market","allowedSources":[]}"#, "Marketplace 'market' is not in the allowed marketplace list"),
            (r#"{"source":"source","type":"marketplace-blocked-by-policy","marketplace":"market","blockedByBlocklist":false,"allowedSources":[]}"#, "Marketplace 'market' is not in the allowed marketplace list"),
            (r#"{"source":"source","type":"marketplace-blocked-by-policy","marketplace":"market","blockedByBlocklist":true,"allowedSources":[]}"#, "Marketplace 'market' is blocked by enterprise policy"),
            (r#"{"source":"source","type":"dependency-unsatisfied","plugin":"p","dependency":"dep","reason":"not-enabled"}"#, "Dependency \"dep\" is disabled — enable it or remove the dependency"),
            (r#"{"source":"source","type":"dependency-unsatisfied","plugin":"p","dependency":"dep","reason":"not-found"}"#, "Dependency \"dep\" is not found in any configured marketplace"),
            (r#"{"source":"source","type":"plugin-cache-miss","plugin":"p","installPath":"/cache"}"#, "Plugin \"p\" not cached at /cache — run /plugins to refresh"),
        ];
        for (input, expected) in cases {
            let error: PluginError = serde_json::from_str(input).unwrap();
            assert_eq!(get_plugin_error_message(&error), *expected, "{input}");
            assert_eq!(error.source(), "source");
            let value = serde_json::to_value(&error).unwrap();
            let original: serde_json::Value = serde_json::from_str(input).unwrap();
            // JSON numeric spellings can differ, but the same Number value is retained.
            let decoded: PluginError = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(decoded, error);
            let mut original_keys: Vec<_> = original.as_object().unwrap().keys().collect();
            let mut keys: Vec<_> = value.as_object().unwrap().keys().collect();
            original_keys.sort();
            keys.sort();
            assert_eq!(keys, original_keys, "{input}");
            for (key, original_value) in original.as_object().unwrap() {
                if original_value.is_number() {
                    assert_eq!(value[key].as_f64(), original_value.as_f64(), "{input}");
                } else {
                    assert_eq!(value[key], *original_value, "{input}");
                }
            }
        }
    }

    /// CC `number` also admits values JSON cannot transport; use direct carriers.
    #[test]
    fn plugin_error_nonfinite_numbers_match_official_oracle() {
        for (number, expected) in [
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (-0.0, "0"),
        ] {
            let crash = PluginError::LspServerCrashed {
                source: "source".into(),
                plugin: "p".into(),
                server_name: "s".into(),
                exit_code: Some(number),
                signal: None,
            };
            assert_eq!(
                get_plugin_error_message(&crash),
                format!("Plugin \"p\" LSP server \"s\" crashed with exit code {expected}")
            );
            let timeout = PluginError::LspRequestTimeout {
                source: "source".into(),
                plugin: "p".into(),
                server_name: "s".into(),
                method: "initialize".into(),
                timeout_ms: number,
            };
            assert_eq!(
                get_plugin_error_message(&timeout),
                format!(
                    "Plugin \"p\" LSP server \"s\" timed out on initialize request after {expected}ms"
                )
            );
        }
    }
}
