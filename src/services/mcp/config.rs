//! Maps to: CC `services/mcp/config.ts` read-side config helpers.
//!
//! Scoped configuration parsing and asynchronous source aggregation. Synchronous
//! legacy UI consumers bridge to the same canonical asynchronous aggregate.

use super::types::{ConfigScope, ScopedMcpServerConfig, Transport};
use crate::utils::config::{GlobalConfig, McpServerConfig, ProjectConfig};
use crate::utils::settings::constants::SettingSource;
use crate::utils::settings::types::SettingsJson;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CCR_PROXY_PATH_MARKERS: &[&str] = &["/v2/session_ingress/shttp/mcp/", "/v2/ccr-sessions/"];

/// Maps to: CC `ValidationError.mcpErrorMetadata.severity` for MCP configs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConfigErrorSeverity {
    Fatal,
    Warning,
}

impl McpConfigErrorSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Warning => "warning",
        }
    }
}

/// Maps to: CC `ValidationError.mcpErrorMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigErrorMetadata {
    pub scope: ConfigScope,
    pub server_name: Option<String>,
    pub severity: McpConfigErrorSeverity,
}

/// Maps to: CC `utils/settings/validation.ts#ValidationError` as produced by
/// `services/mcp/config.ts#parseMcpConfig(...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigValidationError {
    pub file: Option<String>,
    pub path: String,
    pub message: String,
    pub suggestion: Option<String>,
    pub mcp_error_metadata: McpConfigErrorMetadata,
}

impl McpConfigValidationError {
    pub fn is_missing_file(&self) -> bool {
        self.message.starts_with("MCP config file not found")
    }
}

/// Maps to: CC `getMcpConfigsByScope(scope)` return shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpConfigsByScope {
    pub servers: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    pub errors: Vec<McpConfigValidationError>,
}

/// Maps to the anonymous result of CC getClaudeCodeMcpConfigs/getAllMcpConfigs.
/// Plugin loading errors are distinct from per-file settings validation errors.
#[derive(Clone, Debug, Default)]
pub struct McpConfigs {
    pub servers: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    pub errors: Vec<crate::types::plugin::PluginError>,
}

fn mcp_config_error(
    scope: ConfigScope,
    file: Option<&Path>,
    path: impl Into<String>,
    message: impl Into<String>,
    severity: McpConfigErrorSeverity,
    server_name: Option<&str>,
    suggestion: Option<String>,
) -> McpConfigValidationError {
    McpConfigValidationError {
        file: file.map(|path| path.display().to_string()),
        path: path.into(),
        message: message.into(),
        suggestion,
        mcp_error_metadata: McpConfigErrorMetadata {
            scope,
            server_name: server_name.map(str::to_string),
            severity,
        },
    }
}

fn percent_decode_query_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                out.push(byte);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Maps to: CC `services/mcp/config.ts#unwrapCcrProxyUrl`.
pub fn unwrap_ccr_proxy_url(url: &str) -> String {
    if !CCR_PROXY_PATH_MARKERS
        .iter()
        .any(|marker| url.contains(marker))
    {
        return url.to_string();
    }

    // Maps to CC `new URL(url)` guard: malformed proxy strings keep their
    // original value instead of trusting an arbitrary `mcp_url` substring.
    let Some(scheme_separator) = url.find("://") else {
        return url.to_string();
    };
    let after_scheme = &url[scheme_separator + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    if scheme_separator == 0 || authority_end == 0 || url.chars().any(char::is_whitespace) {
        return url.to_string();
    }

    let Some(query) = url.split_once('?').map(|(_, query)| query) else {
        return url.to_string();
    };
    for part in query.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key == "mcp_url" {
            return percent_decode_query_value(value).unwrap_or_else(|| value.to_string());
        }
    }
    url.to_string()
}

fn scoped_server_command_array(config: &ScopedMcpServerConfig) -> Option<Vec<String>> {
    if config.transport != Transport::Stdio {
        return None;
    }
    let command = config.command.as_ref()?;
    Some(
        std::iter::once(command.clone())
            .chain(config.args.iter().cloned())
            .collect(),
    )
}

/// Maps to: CC `services/mcp/config.ts#getMcpServerSignature`.
pub fn get_mcp_server_signature(config: &ScopedMcpServerConfig) -> Option<String> {
    if let Some(command) = scoped_server_command_array(config) {
        let encoded = serde_json::to_string(&command).unwrap_or_else(|_| "[]".to_string());
        return Some(format!("stdio:{encoded}"));
    }
    config
        .url
        .as_deref()
        .map(|url| format!("url:{}", unwrap_ccr_proxy_url(url)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerDedupSuppression {
    pub name: String,
    pub duplicate_of: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpServerDedupResult {
    pub servers: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    pub suppressed: Vec<McpServerDedupSuppression>,
}

/// Maps to: CC `services/mcp/config.ts#dedupPluginMcpServers`.
pub fn dedup_plugin_mcp_servers_readonly(
    plugin_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    manual_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> McpServerDedupResult {
    let mut manual_signatures = BTreeMap::<String, String>::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(manual_servers) {
        if let Some(signature) = get_mcp_server_signature(config) {
            manual_signatures
                .entry(signature)
                .or_insert_with(|| name.to_owned());
        }
    }

    let mut servers = indexmap::IndexMap::new();
    let mut suppressed = Vec::new();
    let mut seen_plugin_signatures = BTreeMap::<String, String>::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(plugin_servers) {
        let Some(signature) = get_mcp_server_signature(config) else {
            servers.insert(name.to_owned(), config.clone());
            continue;
        };
        if let Some(duplicate_of) = manual_signatures.get(&signature) {
            crate::utils::debug::log_for_debugging(&format!(
                "Suppressing plugin MCP server \"{name}\": duplicates \"{duplicate_of}\""
            ));
            suppressed.push(McpServerDedupSuppression {
                name: name.to_owned(),
                duplicate_of: duplicate_of.clone(),
            });
            continue;
        }
        if let Some(duplicate_of) = seen_plugin_signatures.get(&signature) {
            crate::utils::debug::log_for_debugging(&format!(
                "Suppressing plugin MCP server \"{name}\": duplicates \"{duplicate_of}\""
            ));
            suppressed.push(McpServerDedupSuppression {
                name: name.to_owned(),
                duplicate_of: duplicate_of.clone(),
            });
            continue;
        }
        seen_plugin_signatures.insert(signature, name.to_owned());
        servers.insert(name.to_owned(), config.clone());
    }

    McpServerDedupResult {
        servers,
        suppressed,
    }
}

/// Maps to: CC `services/mcp/config.ts#dedupClaudeAiMcpServers`.
pub fn dedup_claude_ai_mcp_servers_readonly(
    claude_ai_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    manual_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    project_config: &ProjectConfig,
) -> McpServerDedupResult {
    let mut manual_signatures = BTreeMap::<String, String>::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(manual_servers) {
        if is_mcp_server_disabled_in_project_config(name, project_config) {
            continue;
        }
        if let Some(signature) = get_mcp_server_signature(config) {
            manual_signatures
                .entry(signature)
                .or_insert_with(|| name.to_owned());
        }
    }

    let mut servers = indexmap::IndexMap::new();
    let mut suppressed = Vec::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(claude_ai_servers) {
        let Some(signature) = get_mcp_server_signature(config) else {
            servers.insert(name.to_owned(), config.clone());
            continue;
        };
        if let Some(duplicate_of) = manual_signatures.get(&signature) {
            crate::utils::debug::log_for_debugging(&format!(
                "Suppressing claude.ai connector \"{name}\": duplicates \"{duplicate_of}\""
            ));
            suppressed.push(McpServerDedupSuppression {
                name: name.to_owned(),
                duplicate_of: duplicate_of.clone(),
            });
            continue;
        }
        servers.insert(name.to_owned(), config.clone());
    }

    McpServerDedupResult {
        servers,
        suppressed,
    }
}

fn scoped_server_url(config: &ScopedMcpServerConfig) -> Option<&str> {
    config.url.as_deref()
}

/// Maps to: CC `services/mcp/config.ts#urlPatternToRegex`.
pub fn url_pattern_to_regex(pattern: &str) -> regex::Regex {
    let mut regex_pattern = String::from("^");
    for ch in pattern.chars() {
        if ch == '*' {
            regex_pattern.push_str(".*");
        } else {
            regex_pattern.push_str(&regex::escape(&ch.to_string()));
        }
    }
    regex_pattern.push('$');
    regex::Regex::new(&regex_pattern).expect("escaped MCP URL pattern must compile")
}

/// Maps to: CC `services/mcp/config.ts#urlMatchesPattern`.
pub fn url_matches_pattern(url: &str, pattern: &str) -> bool {
    url_pattern_to_regex(pattern).is_match(url)
}

fn command_arrays_match(a: &[String], b: &[String]) -> bool {
    a == b
}

/// Maps to: CC `services/mcp/config.ts#shouldAllowManagedMcpServersOnly`.
pub fn should_allow_managed_mcp_servers_only_from_policy(
    policy_settings: Option<&SettingsJson>,
) -> bool {
    policy_settings.and_then(|settings| settings.allow_managed_mcp_servers_only) == Some(true)
}

/// Maps to: CC `services/mcp/config.ts#shouldAllowManagedMcpServersOnly`.
pub fn should_allow_managed_mcp_servers_only() -> bool {
    should_allow_managed_mcp_servers_only_from_policy(
        crate::utils::settings::get_settings_for_source(SettingSource::Policy).as_ref(),
    )
}

fn is_mcp_server_denied_with_settings(
    server_name: &str,
    config: Option<&ScopedMcpServerConfig>,
    settings: &SettingsJson,
) -> bool {
    let Some(denied) = settings.denied_mcp_servers.as_ref() else {
        return false;
    };

    if denied
        .iter()
        .any(|entry| entry.server_name.as_deref() == Some(server_name))
    {
        return true;
    }

    if let Some(config) = config {
        if let Some(server_command) = scoped_server_command_array(config) {
            if denied.iter().any(|entry| {
                entry
                    .server_command
                    .as_ref()
                    .is_some_and(|command| command_arrays_match(command, &server_command))
            }) {
                return true;
            }
        }

        if let Some(server_url) = scoped_server_url(config) {
            if denied.iter().any(|entry| {
                entry
                    .server_url
                    .as_ref()
                    .is_some_and(|pattern| url_matches_pattern(server_url, pattern))
            }) {
                return true;
            }
        }
    }

    false
}

fn is_mcp_server_allowed_by_policy_with_settings(
    server_name: &str,
    config: Option<&ScopedMcpServerConfig>,
    allowlist_settings: &SettingsJson,
    denylist_settings: &SettingsJson,
) -> bool {
    if is_mcp_server_denied_with_settings(server_name, config, denylist_settings) {
        return false;
    }

    let Some(allowed) = allowlist_settings.allowed_mcp_servers.as_ref() else {
        return true;
    };
    if allowed.is_empty() {
        return false;
    }

    let name_allowed = || {
        allowed
            .iter()
            .any(|entry| entry.server_name.as_deref() == Some(server_name))
    };

    let Some(config) = config else {
        return name_allowed();
    };

    let has_command_entries = allowed.iter().any(|entry| entry.server_command.is_some());
    let has_url_entries = allowed.iter().any(|entry| entry.server_url.is_some());

    if let Some(server_command) = scoped_server_command_array(config) {
        if has_command_entries {
            return allowed.iter().any(|entry| {
                entry
                    .server_command
                    .as_ref()
                    .is_some_and(|command| command_arrays_match(command, &server_command))
            });
        }
        return name_allowed();
    }

    if let Some(server_url) = scoped_server_url(config) {
        if has_url_entries {
            return allowed.iter().any(|entry| {
                entry
                    .server_url
                    .as_ref()
                    .is_some_and(|pattern| url_matches_pattern(server_url, pattern))
            });
        }
        return name_allowed();
    }

    name_allowed()
}

/// Maps to: CC `services/mcp/config.ts#isMcpServerAllowedByPolicy`.
pub fn is_mcp_server_allowed_by_policy_readonly(
    server_name: &str,
    config: Option<&ScopedMcpServerConfig>,
) -> bool {
    let policy_settings = crate::utils::settings::get_settings_for_source(SettingSource::Policy);
    let allowlist_settings =
        if should_allow_managed_mcp_servers_only_from_policy(policy_settings.as_ref()) {
            policy_settings.unwrap_or_default()
        } else {
            crate::utils::settings::get_initial_settings()
        };
    let denylist_settings = crate::utils::settings::get_initial_settings();
    is_mcp_server_allowed_by_policy_with_settings(
        server_name,
        config,
        &allowlist_settings,
        &denylist_settings,
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpPolicyFilterResult {
    pub allowed: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    pub blocked: Vec<String>,
}

/// Maps to: CC `services/mcp/config.ts#filterMcpServersByPolicy`.
pub fn filter_mcp_servers_by_policy_with_settings(
    configs: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    allowlist_settings: &SettingsJson,
    denylist_settings: &SettingsJson,
) -> McpPolicyFilterResult {
    let mut allowed = indexmap::IndexMap::new();
    let mut blocked = Vec::new();
    for (name, config) in crate::utils::process_env::ecmascript_object_entries(&configs)
        .into_iter()
        .map(|(name, config)| (name.to_owned(), config.clone()))
    {
        if config.transport == Transport::Sdk
            || is_mcp_server_allowed_by_policy_with_settings(
                &name,
                Some(&config),
                allowlist_settings,
                denylist_settings,
            )
        {
            allowed.insert(name, config);
        } else {
            blocked.push(name);
        }
    }
    McpPolicyFilterResult { allowed, blocked }
}

/// Maps to: CC `services/mcp/config.ts#filterMcpServersByPolicy`.
pub fn filter_mcp_servers_by_policy_readonly(
    configs: indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> McpPolicyFilterResult {
    let policy_settings = crate::utils::settings::get_settings_for_source(SettingSource::Policy);
    let allowlist_settings =
        if should_allow_managed_mcp_servers_only_from_policy(policy_settings.as_ref()) {
            policy_settings.unwrap_or_default()
        } else {
            crate::utils::settings::get_initial_settings()
        };
    let denylist_settings = crate::utils::settings::get_initial_settings();
    filter_mcp_servers_by_policy_with_settings(configs, &allowlist_settings, &denylist_settings)
}

fn config_uses_windows_npx_without_cmd(
    config: &McpServerConfig,
    platform: crate::utils::env::Platform,
) -> bool {
    if platform != crate::utils::env::Platform::Windows {
        return false;
    }
    if super::types::transport_from_config(config) != Transport::Stdio {
        return false;
    }
    let Some(command) = config.command.as_deref() else {
        return false;
    };
    command == "npx" || command.ends_with("\\npx") || command.ends_with("/npx")
}

fn expand_config_env_vars(mut config: McpServerConfig) -> (McpServerConfig, Vec<String>) {
    let mut missing = Vec::<String>::new();
    let mut expand = |value: String| {
        let (expanded, vars) =
            crate::services::mcp::env_expansion::expand_env_vars_in_string(&value);
        missing.extend(vars);
        expanded
    };

    match super::types::transport_from_config(&config) {
        Transport::Stdio => {
            config.command = config.command.map(&mut expand);
            config.args = config
                .args
                .map(|args| args.into_iter().map(&mut expand).collect());
            config.env = config.env.map(|env| {
                env.into_iter()
                    .map(|(key, value)| (key, expand(value)))
                    .collect()
            });
        }
        Transport::Sse | Transport::Http | Transport::Ws => {
            config.url = config.url.map(&mut expand);
            config.headers = config.headers.map(|headers| {
                headers
                    .into_iter()
                    .map(|(key, value)| (key, expand(value)))
                    .collect()
            });
        }
        Transport::SseIde | Transport::WsIde | Transport::Sdk | Transport::ClaudeAiProxy => {}
    }

    let mut seen = BTreeSet::new();
    missing.retain(|var| seen.insert(var.clone()));
    (config, missing)
}

/// Maps to: CC `services/mcp/config.ts#parseMcpConfig`.
pub fn parse_mcp_config_value_readonly(
    config_object: &Value,
    expand_vars: bool,
    scope: ConfigScope,
    file_path: Option<&Path>,
) -> McpConfigsByScope {
    let mut errors = Vec::new();
    let validated = match crate::utils::zod::safe_parse(
        super::types::mcp_json_config_schema(),
        config_object,
    ) {
        Ok(validated) => validated,
        Err(error) => {
            errors.extend(error.issues.into_iter().map(|issue| {
                let path = issue
                    .path
                    .iter()
                    .map(|part| match part {
                        crate::utils::zod::PathSegment::Key(key) => key.clone(),
                        crate::utils::zod::PathSegment::Index(index) => index.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                mcp_config_error(
                    scope,
                    file_path,
                    path,
                    "Does not adhere to MCP server configuration schema",
                    McpConfigErrorSeverity::Fatal,
                    None,
                    None,
                )
            }));
            return McpConfigsByScope {
                servers: indexmap::IndexMap::new(),
                errors,
            };
        }
    };
    let mut raw_servers = indexmap::IndexMap::<String, McpServerConfig>::new();
    for (name, value) in crate::utils::process_env::ecmascript_object_entries(
        validated["mcpServers"]
            .as_object()
            .expect("canonical MCP config object"),
    ) {
        let config: McpServerConfig = serde_json::from_value(value.clone())
            .expect("canonical MCP schema output fits the typed carrier");
        raw_servers.insert(name.to_owned(), config);
    }

    let mut servers = indexmap::IndexMap::new();
    for (name, config) in raw_servers {
        let (config, missing_vars) = if expand_vars {
            expand_config_env_vars(config)
        } else {
            (config, Vec::new())
        };
        if !missing_vars.is_empty() {
            errors.push(mcp_config_error(
                scope,
                file_path,
                format!("mcpServers.{name}"),
                format!("Missing environment variables: {}", missing_vars.join(", ")),
                McpConfigErrorSeverity::Warning,
                Some(&name),
                Some(format!(
                    "Set the following environment variables: {}",
                    missing_vars.join(", ")
                )),
            ));
        }
        if config_uses_windows_npx_without_cmd(&config, crate::utils::env::get().platform) {
            errors.push(mcp_config_error(
                scope,
                file_path,
                format!("mcpServers.{name}"),
                "Windows requires 'cmd /c' wrapper to execute npx",
                McpConfigErrorSeverity::Warning,
                Some(&name),
                Some("Change command to \"cmd\" with args [\"/c\", \"npx\", ...]. See: https://code.claude.com/docs/en/mcp#configure-mcp-servers".to_string()),
            ));
        }
        servers.insert(name, ScopedMcpServerConfig::from_config(scope, &config));
    }

    McpConfigsByScope { servers, errors }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DynamicMcpConfigResult {
    pub servers: indexmap::IndexMap<String, ScopedMcpServerConfig>,
    pub errors: Vec<McpConfigValidationError>,
    pub blocked: Vec<String>,
}

fn json_value_is_js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Maps to CC `main.tsx` variadic `--mcp-config <configs...>` parsing. Each
/// item is first interpreted as inline JSON, then as a file path; later valid
/// items override earlier server names.
pub fn parse_dynamic_mcp_configs_readonly(items: &[String]) -> DynamicMcpConfigResult {
    let mut servers = indexmap::IndexMap::new();
    let mut errors = Vec::new();

    for item in items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
    {
        let parsed = match serde_json::from_str::<Value>(item) {
            Ok(value) if json_value_is_js_truthy(&value) => parse_mcp_config_value_readonly(
                &value,
                true,
                ConfigScope::Dynamic,
                Some(Path::new("command line")),
            ),
            Ok(_) | Err(_) => {
                let path = PathBuf::from(item);
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir().unwrap_or_default().join(path)
                };
                parse_mcp_config_from_file_path_readonly(&path, true, ConfigScope::Dynamic)
            }
        };
        if parsed.errors.is_empty() {
            servers.extend(parsed.servers);
        } else {
            errors.extend(parsed.errors);
        }
    }

    if !errors.is_empty() {
        return DynamicMcpConfigResult {
            servers: indexmap::IndexMap::new(),
            errors,
            blocked: Vec::new(),
        };
    }

    let filtered = filter_mcp_servers_by_policy_readonly(servers);
    DynamicMcpConfigResult {
        servers: filtered.allowed,
        errors,
        blocked: filtered.blocked,
    }
}

pub fn format_dynamic_mcp_config_errors(errors: &[McpConfigValidationError]) -> String {
    errors
        .iter()
        .map(|error| {
            if error.path.is_empty() {
                error.message.clone()
            } else {
                format!("{}: {}", error.path, error.message)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps to: CC `services/mcp/config.ts#parseMcpConfigFromFilePath`.
pub fn parse_mcp_config_from_file_path_readonly(
    file_path: &Path,
    expand_vars: bool,
    scope: ConfigScope,
) -> McpConfigsByScope {
    let content = match crate::utils::fs_operations::get_fs_implementation()
        .read_file_sync(file_path, crate::utils::fs_operations::BufferEncoding::Utf8)
        .map(|text| text.to_string_lossy())
    {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return McpConfigsByScope {
                servers: indexmap::IndexMap::new(),
                errors: vec![mcp_config_error(
                    scope,
                    Some(file_path),
                    "",
                    format!("MCP config file not found: {}", file_path.display()),
                    McpConfigErrorSeverity::Fatal,
                    None,
                    Some("Check that the file path is correct".to_string()),
                )],
            };
        }
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "MCP config read error for {}: {error}",
                file_path.display()
            ));
            return McpConfigsByScope {
                servers: indexmap::IndexMap::new(),
                errors: vec![mcp_config_error(
                    scope,
                    Some(file_path),
                    "",
                    format!("Failed to read file: {error}"),
                    McpConfigErrorSeverity::Fatal,
                    None,
                    Some("Check file permissions and ensure the file exists".to_string()),
                )],
            };
        }
    };

    let parsed = match serde_json::from_str::<Value>(&content) {
        Ok(parsed) => parsed,
        Err(_) => {
            crate::utils::debug::log_for_debugging(&format!(
                "MCP config for {} is not valid JSON",
                file_path.display()
            ));
            return McpConfigsByScope {
                servers: indexmap::IndexMap::new(),
                errors: vec![mcp_config_error(
                    scope,
                    Some(file_path),
                    "",
                    "MCP config is not a valid JSON",
                    McpConfigErrorSeverity::Fatal,
                    None,
                    Some("Fix the JSON syntax errors in the file".to_string()),
                )],
            };
        }
    };

    parse_mcp_config_value_readonly(&parsed, expand_vars, scope, Some(file_path))
}

fn mcp_configs_from_snapshot(scope: ConfigScope, source: Option<&Value>) -> McpConfigsByScope {
    // Source getMcpConfigsByScope returns empty before parsing when the slot
    // is absent or falsy; otherwise it passes the original object unchanged.
    let Some(servers) = source.filter(|value| json_value_is_js_truthy(value)) else {
        return McpConfigsByScope::default();
    };
    parse_mcp_config_value_readonly(
        &serde_json::json!({ "mcpServers": servers }),
        true,
        scope,
        None,
    )
}

fn current_project_mcp_dirs() -> Vec<PathBuf> {
    let Ok(mut current) = std::env::current_dir() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    loop {
        if current.parent().is_none() {
            break;
        }
        dirs.push(current.clone());
        let Some(parent) = current.parent() else {
            break;
        };
        let parent = parent.to_path_buf();
        if parent == current {
            break;
        }
        current = parent;
    }
    dirs.reverse();
    dirs
}

/// Maps to: CC `getMcpConfigsByScope(...)` for the config sources Cometix
/// already has in memory plus read-only `.mcp.json`/enterprise file parsing.
pub fn get_mcp_configs_by_scope_readonly(
    scope: ConfigScope,
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
) -> McpConfigsByScope {
    match scope {
        ConfigScope::User => mcp_configs_from_snapshot(scope, global_config.mcp_servers.as_ref()),
        ConfigScope::Local => mcp_configs_from_snapshot(scope, project_config.mcp_servers.as_ref()),
        ConfigScope::Project => {
            let mut result = McpConfigsByScope::default();
            for dir in current_project_mcp_dirs() {
                let file_path = dir.join(".mcp.json");
                let parsed = parse_mcp_config_from_file_path_readonly(&file_path, true, scope);
                let non_missing_errors = parsed
                    .errors
                    .into_iter()
                    .filter(|error| !error.is_missing_file())
                    .collect::<Vec<_>>();
                result.errors.extend(non_missing_errors);
                result.servers.extend(parsed.servers);
            }
            result
        }
        ConfigScope::Enterprise => {
            let parsed = parse_mcp_config_from_file_path_readonly(
                &super::utils::get_enterprise_mcp_file_path_readonly(),
                true,
                scope,
            );
            let errors = parsed
                .errors
                .into_iter()
                .filter(|error| !error.is_missing_file())
                .collect::<Vec<_>>();
            McpConfigsByScope {
                servers: parsed.servers,
                errors,
            }
        }
        ConfigScope::Dynamic | ConfigScope::ClaudeAi | ConfigScope::Managed => {
            McpConfigsByScope::default()
        }
    }
}

/// Maps to: CC `services/mcp/config.ts:1146-1163#getClaudeCodeMcpConfigs`.
/// The source Promise.all remains parallel and uses each plugin's unresolved
/// getPluginMcpServers cache. Legacy synchronous callers bridge once outside.
async fn plugin_mcp_servers() -> anyhow::Result<McpConfigs> {
    let plugin_result = crate::utils::plugins::plugin_loader::load_all_plugins_cache_only().await?;
    // config.ts:1117-1143: report loader errors before activating servers.
    // Availability failures are debug-only; MCP loading failures are errors.
    for error in &plugin_result.errors.snapshot() {
        let encoded = serde_json::to_value(error).expect("plugin error serializes");
        let kind = encoded["type"]
            .as_str()
            .expect("plugin error discriminator");
        if matches!(
            kind,
            "mcp-config-invalid"
                | "mcpb-download-failed"
                | "mcpb-extract-failed"
                | "mcpb-invalid-manifest"
        ) {
            crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
                "Plugin MCP loading error - {kind}: {}",
                crate::types::plugin::get_plugin_error_message(error)
            )));
        } else {
            crate::utils::debug::log_for_debugging(&format!(
                "Plugin not available for MCP: {} - error type: {kind}",
                error.source()
            ));
        }
    }
    let errors = std::sync::Mutex::new(Vec::new());
    let results = futures::future::join_all(plugin_result.enabled.iter().map(|plugin| {
        crate::utils::plugins::mcp_plugin_integration::get_plugin_mcp_servers(plugin, &errors)
    }))
    .await;
    let mut servers = indexmap::IndexMap::new();
    for result in results.into_iter().flatten() {
        servers.extend(result);
    }
    let errors = errors.into_inner().unwrap();
    for error in &errors {
        let encoded = serde_json::to_value(&error).expect("plugin error serializes");
        crate::utils::log::log_error(crate::utils::log::LogError::new(format!(
            "Plugin MCP server error - {}: {}",
            encoded["type"]
                .as_str()
                .expect("plugin error discriminator"),
            crate::types::plugin::get_plugin_error_message(&error)
        )));
    }
    Ok(McpConfigs { servers, errors })
}

/// Maps to: CC `services/mcp/config.ts#getClaudeCodeMcpConfigs` split of
/// `enabledPluginServers` and `disabledPluginServers` before
/// `dedupPluginMcpServers(...)`. Disabled/policy-blocked plugin servers still
/// appear in `/mcp`, but they must not win the first-plugin-wins race against
/// enabled duplicates.
fn dedup_plugin_mcp_servers_for_get_claude_code_configs_readonly(
    plugin_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    enabled_manual_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    project_config: &ProjectConfig,
) -> McpServerDedupResult {
    let mut enabled_plugin_servers = indexmap::IndexMap::new();
    let mut disabled_plugin_servers = indexmap::IndexMap::new();
    for (name, config) in plugin_servers {
        if is_mcp_server_disabled_in_project_config(name, project_config)
            || !is_mcp_server_allowed_by_policy_readonly(name, Some(config))
        {
            disabled_plugin_servers.insert(name.clone(), config.clone());
        } else {
            enabled_plugin_servers.insert(name.clone(), config.clone());
        }
    }

    let mut result =
        dedup_plugin_mcp_servers_readonly(&enabled_plugin_servers, enabled_manual_servers);
    result.servers.extend(disabled_plugin_servers);
    result
}

/// Maps to: CC `services/mcp/config.ts#doesEnterpriseMcpConfigExist`.
pub fn does_enterprise_mcp_config_exist_readonly() -> bool {
    super::utils::get_enterprise_mcp_file_path_readonly().is_file()
}

/// Shared/eager JS Promise carrier at the source's fetch call sites. This
/// carries the already-existing claude.ai fetch, without another fetch/cache.
pub(super) type McpConfigPromise = futures::future::Shared<
    futures::future::BoxFuture<'static, indexmap::IndexMap<String, ScopedMcpServerConfig>>,
>;

pub(super) fn start_mcp_config_promise(
    future: impl std::future::Future<Output = indexmap::IndexMap<String, ScopedMcpServerConfig>>
    + Send
    + 'static,
) -> McpConfigPromise {
    use futures::FutureExt;
    let promise = future.boxed().shared();
    let worker = promise.clone();
    crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("MCP discovery requires the published process runtime")
        .spawn(async move {
            let _ = worker.await;
        });
    promise
}

/// Existing synchronous caller bridge; the aggregation algorithm remains in
/// the source-owned asynchronous getClaudeCodeMcpConfigs below.
pub fn get_claude_code_mcp_configs_readonly(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
    dynamic_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    extra_dedup_targets: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> McpConfigs {
    let (global, project, dynamic, extra) = (
        global_config.clone(),
        project_config.clone(),
        dynamic_servers.clone(),
        extra_dedup_targets.clone(),
    );
    match crate::utils::process_runtime::block_on_from_sync(async move {
        get_claude_code_mcp_configs(&global, &project, &dynamic, futures::future::ready(extra))
            .await
    }) {
        Some(Ok(result)) => result,
        result => McpConfigs {
            servers: Default::default(),
            errors: vec![crate::types::plugin::PluginError::GenericError {
                source: "plugins".into(),
                plugin: None,
                error: match result {
                    Some(Err(error)) => error.to_string(),
                    _ => "Unable to initialize plugin loading runtime".into(),
                },
            }],
        },
    }
}

/// Maps to: CC `services/mcp/config.ts:1070#getClaudeCodeMcpConfigs`.
pub async fn get_claude_code_mcp_configs(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
    dynamic_servers: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    extra_dedup_targets: impl std::future::Future<
        Output = indexmap::IndexMap<String, ScopedMcpServerConfig>,
    >,
) -> anyhow::Result<McpConfigs> {
    let enterprise_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Enterprise, global_config, project_config)
            .servers;
    if does_enterprise_mcp_config_exist_readonly() {
        return Ok(McpConfigs {
            servers: enterprise_servers
                .into_iter()
                .filter(|(name, config)| {
                    is_mcp_server_allowed_by_policy_readonly(name, Some(config))
                })
                .collect(),
            errors: Vec::new(),
        });
    }

    let mcp_locked =
        crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("mcp");
    let user_servers = if mcp_locked {
        indexmap::IndexMap::new()
    } else {
        get_mcp_configs_by_scope_readonly(ConfigScope::User, global_config, project_config).servers
    };
    let project_servers = if mcp_locked {
        indexmap::IndexMap::new()
    } else {
        get_mcp_configs_by_scope_readonly(ConfigScope::Project, global_config, project_config)
            .servers
    };
    let local_servers = if mcp_locked {
        indexmap::IndexMap::new()
    } else {
        get_mcp_configs_by_scope_readonly(ConfigScope::Local, global_config, project_config).servers
    };

    let plugin_result = plugin_mcp_servers().await?;
    let mut mcp_errors = plugin_result.errors;
    let plugin_servers = plugin_result.servers;
    let approved_project_servers = project_servers
        .into_iter()
        .filter(|(name, _)| {
            super::utils::get_project_mcp_server_status(name)
                == super::utils::ProjectMcpServerStatus::Approved
        })
        .collect::<indexmap::IndexMap<_, _>>();

    let extra_dedup_targets = extra_dedup_targets.await;
    // config.ts:1180-1195: merge precedence applies before filtering. A
    // blocked overriding value must not expose the older value as a dedup target.
    let manual_servers: indexmap::IndexMap<_, _> = user_servers
        .iter()
        .chain(approved_project_servers.iter())
        .chain(local_servers.iter())
        .chain(dynamic_servers.iter())
        .chain(extra_dedup_targets.iter())
        .map(|(name, config)| (name.clone(), config.clone()))
        .collect();
    let enabled_manual_servers =
        crate::utils::process_env::ecmascript_object_entries(&manual_servers)
            .into_iter()
            .filter(|(name, config)| {
                !is_mcp_server_disabled_in_project_config(name, project_config)
                    && is_mcp_server_allowed_by_policy_readonly(name, Some(config))
            })
            .map(|(name, config)| (name.to_owned(), config.clone()))
            .collect();
    let deduped = dedup_plugin_mcp_servers_for_get_claude_code_configs_readonly(
        &plugin_servers,
        &enabled_manual_servers,
        project_config,
    );
    for suppression in deduped.suppressed {
        let parts: Vec<_> = suppression.name.split(':').collect();
        if parts.first() != Some(&"plugin") || parts.len() < 3 {
            continue;
        }
        mcp_errors.push(
            crate::types::plugin::PluginError::McpServerSuppressedDuplicate {
                source: suppression.name.clone(),
                plugin: parts[1].to_owned(),
                server_name: parts[2..].join(":"),
                duplicate_of: suppression.duplicate_of,
            },
        );
    }
    let mut servers = deduped.servers;
    // Maps to CC merge precedence: plugin < user < approved project < local.
    servers.extend(user_servers);
    servers.extend(approved_project_servers);
    servers.extend(local_servers);

    Ok(McpConfigs {
        // This source loop deliberately has no SDK exemption (unlike the
        // distinct filterMcpServersByPolicy helper used by other callers).
        servers: crate::utils::process_env::ecmascript_object_entries(&servers)
            .into_iter()
            .filter(|(name, config)| is_mcp_server_allowed_by_policy_readonly(name, Some(config)))
            .map(|(name, config)| (name.to_owned(), config.clone()))
            .collect(),
        errors: mcp_errors,
    })
}

/// Maps to: CC `services/mcp/config.ts#getAllMcpConfigs`.
pub async fn get_all_mcp_configs(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
) -> anyhow::Result<McpConfigs> {
    if does_enterprise_mcp_config_exist_readonly() {
        return get_claude_code_mcp_configs(
            global_config,
            project_config,
            &Default::default(),
            futures::future::ready(Default::default()),
        )
        .await;
    }
    // User-authorized L2 (2026-09-15): keep claudeai.rs ported but disconnected
    // from application discovery. Do not read OAuth/login state or fetch org MCPs.
    // Original wiring retained for future re-enablement:
    // let claudeai_promise =
    //     start_mcp_config_promise(super::claudeai::fetch_claude_ai_mcp_configs_if_eligible());
    let claudeai_promise = start_mcp_config_promise(futures::future::ready(Default::default()));
    let claude_code = get_claude_code_mcp_configs(
        global_config,
        project_config,
        &Default::default(),
        claudeai_promise.clone(),
    )
    .await?;
    let claude_ai_allowed = filter_mcp_servers_by_policy_readonly(claudeai_promise.await).allowed;
    let deduped_claude_ai = dedup_claude_ai_mcp_servers_readonly(
        &claude_ai_allowed,
        &claude_code.servers,
        project_config,
    )
    .servers;
    let mut servers = deduped_claude_ai;
    servers.extend(claude_code.servers);
    Ok(McpConfigs {
        servers,
        errors: claude_code.errors,
    })
}

/// Maps to: CC `getClaudeCodeMcpConfigs(...)` callers that need all configured
/// server names grouped by official display scope.
pub fn all_configured_mcp_servers_readonly(
    global_config: &GlobalConfig,
    project_config: &ProjectConfig,
) -> indexmap::IndexMap<String, ScopedMcpServerConfig> {
    get_claude_code_mcp_configs_readonly(
        global_config,
        project_config,
        &indexmap::IndexMap::new(),
        &indexmap::IndexMap::new(),
    )
    .servers
}

/// Maps to: CC `services/mcp/config.ts#getMcpConfigByName`.
///
/// This lookup intentionally follows the official direct source precedence
/// rather than `getClaudeCodeMcpConfigs(...)` aggregation: enterprise wins by
/// name, strict plugin-only exposes enterprise only, then local overrides
/// project overrides user. It does not consult plugin/dynamic/claude.ai
/// sources, approval state, disabled state, policy filters, or signature dedup.
pub fn get_mcp_config_by_name_readonly(name: &str) -> Option<ScopedMcpServerConfig> {
    let global_config = crate::utils::config::load_global_config();
    let project_config = crate::utils::config::get_current_project_config();

    let enterprise_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Enterprise, &global_config, &project_config)
            .servers;

    if crate::utils::settings::plugin_only_policy::is_restricted_to_plugin_only("mcp") {
        return enterprise_servers.get(name).cloned();
    }

    let user_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::User, &global_config, &project_config)
            .servers;
    let project_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Project, &global_config, &project_config)
            .servers;
    let local_servers =
        get_mcp_configs_by_scope_readonly(ConfigScope::Local, &global_config, &project_config)
            .servers;

    enterprise_servers
        .get(name)
        .or_else(|| local_servers.get(name))
        .or_else(|| project_servers.get(name))
        .or_else(|| user_servers.get(name))
        .cloned()
}

/// Maps to: CC `services/mcp/config.ts#isDefaultDisabledBuiltin`.
///
/// The only official default-disabled built-in is behind a bundled feature that
/// Cometix does not compile yet, so this safe projection returns false until the
/// corresponding built-in MCP source is ported.
fn is_default_disabled_builtin(_name: &str) -> bool {
    false
}

/// Maps to: CC `services/mcp/config.ts#isMcpServerDisabled`.
pub fn is_mcp_server_disabled_in_project_config(
    name: &str,
    project_config: &ProjectConfig,
) -> bool {
    if is_default_disabled_builtin(name) {
        return project_config
            .enabled_mcp_servers
            .as_ref()
            .is_none_or(|servers| !servers.iter().any(|server| server == name));
    }
    project_config
        .disabled_mcp_servers
        .as_ref()
        .is_some_and(|servers| servers.iter().any(|server| server == name))
}

/// Maps to: CC `services/mcp/config.ts#isMcpServerDisabled`.
pub fn is_mcp_server_disabled(name: &str) -> bool {
    is_mcp_server_disabled_in_project_config(
        name,
        &crate::utils::config::get_current_project_config(),
    )
}

/// Maps to: CC `services/mcp/config.ts#toggleMembership`.
fn toggle_membership(list: &[String], name: &str, should_contain: bool) -> Vec<String> {
    let contains = list.iter().any(|item| item == name);
    if contains == should_contain {
        return list.to_vec();
    }
    if should_contain {
        list.iter()
            .cloned()
            .chain(std::iter::once(name.to_string()))
            .collect()
    } else {
        list.iter().filter(|item| *item != name).cloned().collect()
    }
}

/// Maps to: CC `services/mcp/config.ts#setMcpServerEnabled`.
///
/// This persists the official project-level enabled/disabled lists through the
/// existing Cometix config writer. `save_current_project_config` still honors
/// the repository write gate (`COMETIX_WRITE_ENABLED`), so production-safe dry
/// runs keep the same no-write behavior as other config updates.
pub fn set_mcp_server_enabled(name: &str, enabled: bool) -> anyhow::Result<()> {
    crate::utils::config::save_current_project_config(|current| {
        if is_default_disabled_builtin(name) {
            let prev = current.enabled_mcp_servers.clone().unwrap_or_default();
            let next = toggle_membership(&prev, name, enabled);
            if next != prev {
                current.enabled_mcp_servers = Some(next);
            }
        } else {
            let prev = current.disabled_mcp_servers.clone().unwrap_or_default();
            let next = toggle_membership(&prev, name, !enabled);
            if next != prev {
                current.disabled_mcp_servers = Some(next);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::McpServerConfig;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Moves the project root to a scratch directory for one test.
    ///
    /// `.mcp.json` and `localSettings` resolve from `get_original_cwd()`
    /// (`utils/settings/mod.rs:54-56`), which is initialised lazily from the
    /// process cwd. Moving only the process cwd therefore worked when the test
    /// ran first and read THIS repository's project configuration once anything
    /// else had already touched `original_cwd`.
    struct ProjectRootGuard {
        previous_cwd: std::path::PathBuf,
        previous_original_cwd: std::path::PathBuf,
    }

    impl ProjectRootGuard {
        fn pin(project_dir: &std::path::Path) -> Self {
            let guard = Self {
                previous_cwd: std::env::current_dir().unwrap(),
                previous_original_cwd: crate::bootstrap::state::get_original_cwd(),
            };
            std::env::set_current_dir(project_dir).unwrap();
            crate::bootstrap::state::set_original_cwd(project_dir);
            crate::utils::settings::settings_cache::reset_settings_cache();
            guard
        }
    }

    impl Drop for ProjectRootGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }

    fn test_scoped_config(
        name: &str,
        transport: Transport,
        command: Option<&str>,
        args: &[&str],
        url: Option<&str>,
    ) -> (String, ScopedMcpServerConfig) {
        (
            name.to_string(),
            ScopedMcpServerConfig {
                name: None,
                scope: ConfigScope::User,
                transport,
                command: command.map(str::to_string),
                args: args.iter().map(|arg| arg.to_string()).collect(),
                env: BTreeMap::new(),
                url: url.map(str::to_string),
                headers: BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                id: None,
                plugin_source: None,
            },
        )
    }

    #[test]
    fn async_local_discovery_waits_at_source_extra_targets_and_has_no_sdk_policy_exemption() {
        let _env = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        // Production effects are polled on the published Tokio runtime. This
        // fixture must enter it before driving the async filesystem path.
        let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work().unwrap();
        let _runtime_context = runtime.enter();
        let root =
            std::env::temp_dir().join(format!("mcp-discovery-config-{}", uuid::Uuid::new_v4()));
        let managed = root.join("managed");
        let config = root.join("config");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _project = ProjectRootGuard::pin(&root);
        std::fs::write(
            managed.join("managed-settings.json"),
            r#"{"deniedMcpServers":[{"serverName":"sdk"}]}"#,
        )
        .unwrap();
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
        let global = GlobalConfig {
            mcp_servers: Some(
                serde_json::json!({"sdk":{"type":"sdk","name":"fixture"},"manual":{"command":"echo"}}),
            ),
            ..Default::default()
        };
        let project = ProjectConfig::default();
        let dynamic = indexmap::IndexMap::new();
        assert!(
            get_mcp_configs_by_scope_readonly(ConfigScope::User, &global, &project)
                .servers
                .contains_key("sdk"),
            "fixture must reach policy rather than fail schema"
        );
        let (resolve, extra) = futures::channel::oneshot::channel();
        let (reached_sender, reached) = std::sync::mpsc::channel();
        let load = get_claude_code_mcp_configs(&global, &project, &dynamic, async move {
            reached_sender.send(()).unwrap();
            extra.await.unwrap()
        });
        let mut load = Box::pin(load);
        futures::executor::block_on(async {
            // Drive real plugin/scopes loading until the actual late extra-target
            // await is reached, then ensure discovery cannot finish prematurely.
            let complete = crate::utils::race(
                async {
                    let result = load.as_mut().await;
                    Some(result)
                },
                async {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    loop {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "actual extra-target await was not reached"
                        );
                        if reached.try_recv().is_ok() {
                            return None;
                        }
                        futures_timer::Delay::new(std::time::Duration::from_millis(1)).await;
                    }
                },
            )
            .await;
            assert!(
                complete.is_none(),
                "local discovery must await provided extra targets"
            );
            resolve.send(indexmap::IndexMap::new()).unwrap();
            let result = load.await.unwrap();
            assert!(result.servers.contains_key("manual"));
            assert!(
                !result.servers.contains_key("sdk"),
                "getClaudeCode's final loop has no SDK exception"
            );
            // Enterprise returns before ever awaiting extra/plugin discovery.
            std::fs::write(
                managed.join("managed-mcp.json"),
                r#"{"mcpServers":{"sdk":{"type":"sdk","name":"fixture"}}}"#,
            )
            .unwrap();
            let result = get_claude_code_mcp_configs(
                &global,
                &project,
                &dynamic,
                futures::future::pending(),
            )
            .await
            .unwrap();
            assert!(
                result.servers.is_empty(),
                "enterprise policy loop also has no SDK exception"
            );
        });
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_mcp_config_canonical_gate_rejects_nulls_and_invalid_oauth_like_bun() {
        // Original McpJsonConfigSchema oracle rejects these inputs. The old
        // union smoke fixture accepted explicit nulls and pinned a hand-written
        // validator bug; its positive cases now omit optional properties.
        for bad in [
            serde_json::json!({"command":"node","args":null}),
            serde_json::json!({"command":"node","env":null}),
            serde_json::json!({"type":"http","url":"","oauth":{"callbackPort":-1}}),
            serde_json::json!({"type":"http","url":"","oauth":null}),
            serde_json::json!({"type":null,"command":"node"}),
        ] {
            let parsed = parse_mcp_config_value_readonly(
                &serde_json::json!({"mcpServers":{"good":{"command":"node"},"bad":bad}}),
                false,
                ConfigScope::User,
                None,
            );
            assert!(
                parsed.servers.is_empty(),
                "one schema failure rejects the entire config"
            );
            assert_eq!(parsed.errors.len(), 1);
            assert_eq!(parsed.errors[0].path, "mcpServers.bad");
            assert_eq!(
                parsed.errors[0].message,
                "Does not adhere to MCP server configuration schema"
            );
            assert_eq!(parsed.errors[0].mcp_error_metadata.server_name, None);
            assert_eq!(
                parsed.errors[0].mcp_error_metadata.severity,
                McpConfigErrorSeverity::Fatal
            );
        }
        let parsed = parse_mcp_config_value_readonly(
            &serde_json::json!({"mcpServers":{"":{"command":"node","extra":1},"http":{"type":"http","url":""}}}),
            false,
            ConfigScope::User,
            None,
        );
        assert!(
            parsed.errors.is_empty(),
            "record keys are not trimmed or restricted by the source"
        );
        assert!(parsed.servers[""].args.is_empty());
        assert_eq!(parsed.servers["http"].url.as_deref(), Some(""));
        let root = parse_mcp_config_value_readonly(
            &serde_json::Value::Null,
            false,
            ConfigScope::User,
            None,
        );
        assert_eq!(root.errors[0].path, "");
    }

    #[test]
    fn parse_mcp_config_accepts_official_union_variants_and_omitted_optionals() {
        let parsed = parse_mcp_config_value_readonly(
            &serde_json::json!({
                "mcpServers": {
                    "stdio": { "command": "node" },
                    "sseIde": {
                        "type": "sse-ide",
                        "url": "http://127.0.0.1:1234/sse",
                        "ideName": "VS Code",
                        "ideRunningInWindows": true
                    },
                    "wsIde": {
                        "type": "ws-ide",
                        "url": "ws://127.0.0.1:1234/ws",
                        "ideName": "JetBrains",
                        "authToken": "secret"
                    },
                    "sdk": { "type": "sdk", "name": "sdk-server" },
                    "claude": {
                        "type": "claudeai-proxy",
                        "url": "https://mcp.example.com/proxy",
                        "id": "connector-id"
                    },
                    "remote": {
                        "type": "http",
                        "url": "https://example.com/mcp"
                    }
                }
            }),
            false,
            ConfigScope::User,
            None,
        );

        assert!(parsed.errors.is_empty(), "errors={:?}", parsed.errors);
        assert_eq!(parsed.servers["stdio"].transport, Transport::Stdio);
        assert_eq!(parsed.servers["sseIde"].transport, Transport::SseIde);
        assert_eq!(
            parsed.servers["sseIde"].ide_name.as_deref(),
            Some("VS Code")
        );
        assert_eq!(parsed.servers["wsIde"].transport, Transport::WsIde);
        assert_eq!(
            parsed.servers["wsIde"].auth_token.as_deref(),
            Some("secret")
        );
        assert_eq!(parsed.servers["sdk"].transport, Transport::Sdk);
        assert_eq!(parsed.servers["sdk"].name.as_deref(), Some("sdk-server"));
        assert!(parsed.servers["sdk"].id.is_none());
        assert_eq!(parsed.servers["sseIde"].ide_running_in_windows, Some(true));
        assert_eq!(parsed.servers["wsIde"].ide_running_in_windows, None);
        assert_eq!(parsed.servers["claude"].transport, Transport::ClaudeAiProxy);
        assert_eq!(parsed.servers["claude"].id.as_deref(), Some("connector-id"));
        assert_eq!(parsed.servers["remote"].transport, Transport::Http);
        assert!(parsed.servers["remote"].headers.is_empty());
        assert!(parsed.servers["remote"].headers_helper.is_none());
        assert!(parsed.servers["remote"].oauth.is_none());
    }

    #[test]
    fn dynamic_mcp_configs_parse_variadic_json_and_later_values_override() {
        let result = parse_dynamic_mcp_configs_readonly(&[
            serde_json::json!({
                "mcpServers": {
                    "docs": { "command": "old" },
                    "first": { "command": "first" }
                }
            })
            .to_string(),
            serde_json::json!({
                "mcpServers": {
                    "docs": { "command": "new" }
                }
            })
            .to_string(),
        ]);

        assert!(result.errors.is_empty(), "errors={:?}", result.errors);
        assert!(result.blocked.is_empty());
        assert_eq!(result.servers.len(), 2);
        assert_eq!(result.servers["docs"].command.as_deref(), Some("new"));
        assert_eq!(result.servers["docs"].scope, ConfigScope::Dynamic);
    }

    #[test]
    fn dynamic_mcp_config_reads_file_paths_like_official_cli() {
        let path =
            std::env::temp_dir().join(format!("cometix-dynamic-mcp-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            serde_json::json!({
                "mcpServers": {
                    "from-file": { "type": "http", "url": "https://example.com/mcp" }
                }
            })
            .to_string(),
        )
        .unwrap();

        let result = parse_dynamic_mcp_configs_readonly(&[path.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_file(path);

        assert!(result.errors.is_empty(), "errors={:?}", result.errors);
        assert_eq!(result.servers["from-file"].transport, Transport::Http);
        assert_eq!(result.servers["from-file"].scope, ConfigScope::Dynamic);
    }

    #[test]
    fn dynamic_mcp_config_invalid_item_fails_the_combined_result() {
        let result = parse_dynamic_mcp_configs_readonly(&[
            serde_json::json!({"mcpServers":{"ok":{"command":"node"}}}).to_string(),
            "{not-json".to_string(),
        ]);

        assert!(result.servers.is_empty());
        assert!(!result.errors.is_empty());
        assert!(
            format_dynamic_mcp_config_errors(&result.errors).contains("MCP config file not found")
        );
    }

    #[test]
    fn mcp_server_signature_unwraps_ccr_proxy_urls_and_dedups_plugins() {
        let manual = indexmap::IndexMap::from([test_scoped_config(
            "docs",
            Transport::Http,
            None,
            &[],
            Some("https://vendor.example/mcp"),
        )]);
        let plugin = indexmap::IndexMap::from([
            test_scoped_config(
                "plugin:toolbox:a",
                Transport::Http,
                None,
                &[],
                Some(
                    "https://ccr.example/v2/session_ingress/shttp/mcp/x?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp",
                ),
            ),
            test_scoped_config(
                "plugin:toolbox:b",
                Transport::Stdio,
                Some("node"),
                &["server.js"],
                None,
            ),
            test_scoped_config(
                "plugin:toolbox:c",
                Transport::Stdio,
                Some("node"),
                &["server.js"],
                None,
            ),
            test_scoped_config("plugin:toolbox:sdk", Transport::Sdk, None, &[], None),
        ]);

        assert_eq!(
            unwrap_ccr_proxy_url(
                "https://ccr.example/v2/ccr-sessions/abc?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp"
            ),
            "https://vendor.example/mcp"
        );
        assert_eq!(
            unwrap_ccr_proxy_url(
                "not a url /v2/ccr-sessions/abc?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp"
            ),
            "not a url /v2/ccr-sessions/abc?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp"
        );
        assert_eq!(
            get_mcp_server_signature(&manual["docs"]).as_deref(),
            Some("url:https://vendor.example/mcp")
        );

        let result = dedup_plugin_mcp_servers_readonly(&plugin, &manual);
        assert!(!result.servers.contains_key("plugin:toolbox:a"));
        assert!(result.servers.contains_key("plugin:toolbox:b"));
        assert!(!result.servers.contains_key("plugin:toolbox:c"));
        assert!(result.servers.contains_key("plugin:toolbox:sdk"));
        assert_eq!(
            result.suppressed,
            vec![
                McpServerDedupSuppression {
                    name: "plugin:toolbox:a".to_string(),
                    duplicate_of: "docs".to_string(),
                },
                McpServerDedupSuppression {
                    name: "plugin:toolbox:c".to_string(),
                    duplicate_of: "plugin:toolbox:b".to_string(),
                }
            ]
        );
    }

    #[test]
    fn dedup_claude_ai_mcp_servers_suppresses_only_enabled_manual_duplicates_like_official() {
        let manual = indexmap::IndexMap::from([
            test_scoped_config(
                "slack",
                Transport::Http,
                None,
                &[],
                Some("https://mcp.slack.com/mcp"),
            ),
            test_scoped_config(
                "disabled-docs",
                Transport::Http,
                None,
                &[],
                Some("https://docs.example.com/mcp"),
            ),
        ]);
        let claude_ai = indexmap::IndexMap::from([
            test_scoped_config(
                "claude.ai Slack",
                Transport::ClaudeAiProxy,
                None,
                &[],
                Some("https://mcp.slack.com/mcp"),
            ),
            test_scoped_config(
                "claude.ai Docs",
                Transport::ClaudeAiProxy,
                None,
                &[],
                Some("https://docs.example.com/mcp"),
            ),
        ]);
        let project = ProjectConfig {
            disabled_mcp_servers: Some(vec!["disabled-docs".to_string()]),
            ..ProjectConfig::default()
        };

        let result = dedup_claude_ai_mcp_servers_readonly(&claude_ai, &manual, &project);

        assert!(!result.servers.contains_key("claude.ai Slack"));
        assert!(result.servers.contains_key("claude.ai Docs"));
        assert_eq!(
            result.suppressed,
            vec![McpServerDedupSuppression {
                name: "claude.ai Slack".to_string(),
                duplicate_of: "slack".to_string(),
            }]
        );
    }

    #[test]
    fn get_all_mcp_configs_preserves_manual_servers_without_claude_ai_business_wiring() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-get-all-no-claudeai-{}",
            uuid::Uuid::new_v4()
        ));
        let config_dir = temp_dir.join("config");
        let managed_dir = temp_dir.join("managed");
        let project_dir = temp_dir.join("project");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_dir);
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let _project_root = ProjectRootGuard::pin(&project_dir);
        crate::utils::config::clear_global_config_cache_for_testing();

        let global = GlobalConfig {
            mcp_servers: Some(
                serde_json::json!({"manual":{"type":"http","url":"https://manual.example/mcp"}}),
            ),
            ..GlobalConfig::default()
        };
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(get_all_mcp_configs(&global, &ProjectConfig::default()))
            .unwrap();

        assert_eq!(
            result.servers["manual"].url.as_deref(),
            Some("https://manual.example/mcp")
        );
        assert!(
            result
                .servers
                .keys()
                .all(|name| !name.starts_with("claude.ai ")),
            "Claude.ai discovery is intentionally disconnected from business entry points"
        );

        drop((_config, _managed, _simple));
        crate::utils::config::clear_global_config_cache_for_testing();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn disabled_plugin_mcp_servers_do_not_suppress_enabled_duplicates_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-disabled-plugin-dedup-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let config_dir = temp_dir.join("config");
        let managed_dir = temp_dir.join("managed");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&managed_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_dir);
        crate::utils::config::clear_global_config_cache_for_testing();

        let plugin_servers = indexmap::IndexMap::from([
            test_scoped_config(
                "plugin:alpha:disabled",
                Transport::Stdio,
                Some("node"),
                &["server.js"],
                None,
            ),
            test_scoped_config(
                "plugin:zeta:enabled",
                Transport::Stdio,
                Some("node"),
                &["server.js"],
                None,
            ),
        ]);
        let project = ProjectConfig {
            disabled_mcp_servers: Some(vec!["plugin:alpha:disabled".to_string()]),
            ..ProjectConfig::default()
        };

        let result = dedup_plugin_mcp_servers_for_get_claude_code_configs_readonly(
            &plugin_servers,
            &indexmap::IndexMap::new(),
            &project,
        );

        assert!(result.servers.contains_key("plugin:alpha:disabled"));
        assert!(result.servers.contains_key("plugin:zeta:enabled"));
        assert!(
            result.suppressed.is_empty(),
            "disabled plugin server must not suppress an enabled duplicate: {:?}",
            result.suppressed
        );

        drop((_config, _managed));
        crate::utils::config::clear_global_config_cache_for_testing();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn mcp_policy_filter_matches_official_allow_deny_and_sdk_exemption() {
        let configs = indexmap::IndexMap::from([
            test_scoped_config("stdioOk", Transport::Stdio, Some("node"), &["ok.js"], None),
            test_scoped_config(
                "stdioDenied",
                Transport::Stdio,
                Some("node"),
                &["denied.js"],
                None,
            ),
            test_scoped_config(
                "remoteOk",
                Transport::Http,
                None,
                &[],
                Some("https://api.example.com/mcp"),
            ),
            test_scoped_config(
                "remoteDenied",
                Transport::Http,
                None,
                &[],
                Some("https://blocked.example.com/mcp"),
            ),
            test_scoped_config("sdkDeniedByNameButExempt", Transport::Sdk, None, &[], None),
        ]);
        let allow_settings = SettingsJson {
            allowed_mcp_servers: Some(vec![
                crate::utils::settings::types::AllowedMcpServerEntry {
                    server_name: None,
                    server_command: Some(vec!["node".to_string(), "ok.js".to_string()]),
                    server_url: None,
                },
                crate::utils::settings::types::AllowedMcpServerEntry {
                    server_name: None,
                    server_command: None,
                    server_url: Some("https://*.example.com/*".to_string()),
                },
            ]),
            ..SettingsJson::default()
        };
        let deny_settings = SettingsJson {
            denied_mcp_servers: Some(vec![
                crate::utils::settings::types::DeniedMcpServerEntry {
                    server_name: None,
                    server_command: Some(vec!["node".to_string(), "denied.js".to_string()]),
                    server_url: None,
                },
                crate::utils::settings::types::DeniedMcpServerEntry {
                    server_name: None,
                    server_command: None,
                    server_url: Some("https://blocked.example.com/*".to_string()),
                },
                crate::utils::settings::types::DeniedMcpServerEntry {
                    server_name: Some("sdkDeniedByNameButExempt".to_string()),
                    server_command: None,
                    server_url: None,
                },
            ]),
            ..SettingsJson::default()
        };

        assert!(url_matches_pattern(
            "https://api.example.com/mcp",
            "https://*.example.com/*"
        ));
        let result =
            filter_mcp_servers_by_policy_with_settings(configs, &allow_settings, &deny_settings);
        // Actual Bun filterMcpServersByPolicy keeps Object.entries order for
        // both outputs; the old alphabetical expectation came from BTreeMap.
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-risk-fixes-0915/policy-order-oracle.json"
        ))
        .unwrap();
        assert_eq!(
            serde_json::json!(
                result
                    .allowed
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            ),
            oracle["allowed"]
        );
        assert_eq!(serde_json::json!(result.blocked), oracle["blocked"]);
    }

    #[test]
    fn managed_mcp_allowlist_flag_reads_policy_settings_only() {
        assert!(!should_allow_managed_mcp_servers_only_from_policy(None));
        assert!(should_allow_managed_mcp_servers_only_from_policy(Some(
            &SettingsJson {
                allow_managed_mcp_servers_only: Some(true),
                ..SettingsJson::default()
            }
        )));
    }

    #[test]
    fn windows_npx_warning_predicate_matches_official_stdio_guard() {
        let stdio_npx = McpServerConfig {
            command: Some("npx".to_string()),
            ..McpServerConfig::default()
        };
        let stdio_path_npx = McpServerConfig {
            command: Some("C:\\tools\\npx".to_string()),
            ..McpServerConfig::default()
        };
        let http_npx = McpServerConfig {
            server_type: Some("http".to_string()),
            command: Some("npx".to_string()),
            url: Some("https://example.com/mcp".to_string()),
            ..McpServerConfig::default()
        };

        assert!(config_uses_windows_npx_without_cmd(
            &stdio_npx,
            crate::utils::env::Platform::Windows
        ));
        assert!(config_uses_windows_npx_without_cmd(
            &stdio_path_npx,
            crate::utils::env::Platform::Windows
        ));
        assert!(!config_uses_windows_npx_without_cmd(
            &stdio_npx,
            crate::utils::env::Platform::MacOS
        ));
        assert!(!config_uses_windows_npx_without_cmd(
            &http_npx,
            crate::utils::env::Platform::Windows
        ));
    }

    #[test]
    fn mcp_snapshot_deserialization_preserves_nulls_sdk_and_source_truthiness() {
        for scope in [ConfigScope::User, ConfigScope::Local] {
            let raw = serde_json::json!({
                "stdio":{"command":"g"},
                "http":{"type":"http","url":"https://manual.example/mcp"},
                "sdk":{"type":"sdk","name":"sdk-host"}
            });
            let global: GlobalConfig =
                serde_json::from_value(serde_json::json!({"mcpServers":raw})).unwrap();
            let project: ProjectConfig =
                serde_json::from_value(serde_json::json!({"mcpServers":raw})).unwrap();
            let result = get_mcp_configs_by_scope_readonly(scope, &global, &project);
            assert!(result.errors.is_empty(), "{:?}", result.errors);
            assert_eq!(result.servers.len(), 3);
            assert_eq!(result.servers["stdio"].command.as_deref(), Some("g"));
            assert_eq!(
                result.servers["http"].url.as_deref(),
                Some("https://manual.example/mcp")
            );
            assert_eq!(result.servers["http"].scope, scope);
            assert_eq!(result.servers["sdk"].name.as_deref(), Some("sdk-host"));

            for bad in [
                serde_json::json!({"command":"g","type":null}),
                serde_json::json!({"command":"g","args":null}),
                serde_json::json!({"command":"g","env":null}),
                serde_json::json!({"type":"http","url":"u","oauth":null}),
                serde_json::json!({"type":"http","url":"u","oauth":{"clientId":null}}),
                serde_json::json!({"type":"http","url":"u","oauth":{"callbackPort":-1}}),
            ] {
                let raw = serde_json::json!({"mcpServers":{"bad":bad,"good":{"command":"g"}}});
                let global: GlobalConfig = serde_json::from_str(&raw.to_string()).unwrap();
                let project: ProjectConfig = serde_json::from_str(&raw.to_string()).unwrap();
                assert_eq!(global.mcp_servers.as_ref().unwrap()["bad"], bad);
                assert_eq!(project.mcp_servers.as_ref().unwrap()["bad"], bad);
                let result = get_mcp_configs_by_scope_readonly(scope, &global, &project);
                assert!(
                    result.servers.is_empty(),
                    "canonical gate remains whole-config"
                );
                assert_eq!(result.errors.len(), 1);
                assert_eq!(result.errors[0].path, "mcpServers.bad");
                assert_eq!(
                    result.errors[0].message,
                    "Does not adhere to MCP server configuration schema"
                );
                assert_eq!(result.errors[0].mcp_error_metadata.scope, scope);
                assert_eq!(
                    result.errors[0].mcp_error_metadata.severity,
                    McpConfigErrorSeverity::Fatal
                );
            }
            for raw in [
                Value::Null,
                serde_json::json!(false),
                serde_json::json!(0),
                serde_json::json!(""),
                serde_json::json!([]),
                serde_json::json!("x"),
            ] {
                let global: GlobalConfig =
                    serde_json::from_value(serde_json::json!({"mcpServers":raw})).unwrap();
                let project: ProjectConfig =
                    serde_json::from_value(serde_json::json!({"mcpServers":raw})).unwrap();
                assert_eq!(global.mcp_servers, Some(raw.clone()));
                assert_eq!(project.mcp_servers, Some(raw.clone()));
                let result = get_mcp_configs_by_scope_readonly(scope, &global, &project);
                assert!(result.servers.is_empty());
                assert_eq!(result.errors.is_empty(), !json_value_is_js_truthy(&raw));
            }
            assert!(mcp_configs_from_snapshot(scope, None).errors.is_empty());
        }
    }

    #[test]
    fn get_mcp_configs_by_scope_projects_user_local_and_mcpjson_sources() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-scope-{}", uuid::Uuid::new_v4()));
        let project_dir = temp_dir.join("project").join("child");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            temp_dir.join("project").join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "project": { "type": "http", "url": "https://project.example/mcp" }
                }
            })
            .to_string(),
        )
        .unwrap();
        let _project_root = ProjectRootGuard::pin(&project_dir);

        let global = GlobalConfig {
            mcp_servers: Some(serde_json::json!({"global":{"command":"g"}})),
            ..GlobalConfig::default()
        };
        let project = ProjectConfig {
            mcp_servers: Some(
                serde_json::json!({"local":{"type":"http","url":"https://local.example/mcp"}}),
            ),
            ..ProjectConfig::default()
        };

        let user = get_mcp_configs_by_scope_readonly(ConfigScope::User, &global, &project);
        assert_eq!(user.servers["global"].scope, ConfigScope::User);
        assert_eq!(user.servers["global"].command.as_deref(), Some("g"));

        let project_scope =
            get_mcp_configs_by_scope_readonly(ConfigScope::Project, &global, &project);
        assert_eq!(project_scope.servers["project"].scope, ConfigScope::Project);
        assert_eq!(
            project_scope.servers["project"].url.as_deref(),
            Some("https://project.example/mcp")
        );

        let local = get_mcp_configs_by_scope_readonly(ConfigScope::Local, &global, &project);
        assert_eq!(local.servers["local"].scope, ConfigScope::Local);
        assert_eq!(
            local.servers["local"].url.as_deref(),
            Some("https://local.example/mcp")
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn all_configured_mcp_servers_filters_project_mcpjson_by_official_approval_status() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-project-approval-{}",
            uuid::Uuid::new_v4()
        ));
        let config_dir = temp_dir.join("config");
        let project_dir = temp_dir.join("project");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(project_dir.join(".claude")).unwrap();
        std::fs::write(
            project_dir.join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "docs": { "type": "http", "url": "https://project.example/mcp" }
                }
            })
            .to_string(),
        )
        .unwrap();

        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let _non_interactive = EnvVarGuard::unset("COMETIX_NON_INTERACTIVE_SESSION");
        let previous_interactive = crate::bootstrap::state::get_is_interactive();
        crate::bootstrap::state::set_is_interactive(true);
        let _project_root = ProjectRootGuard::pin(&project_dir);

        let global = GlobalConfig::default();
        let project = ProjectConfig::default();
        let unapproved = all_configured_mcp_servers_readonly(&global, &project);
        assert!(!unapproved.contains_key("docs"));

        std::fs::write(
            project_dir.join(".claude/settings.local.json"),
            serde_json::json!({ "enabledMcpjsonServers": ["docs"] }).to_string(),
        )
        .unwrap();
        // Writing the file directly bypasses `update_settings_for_source`,
        // which is what normally invalidates the merged-settings cache
        // (settings.ts:505-506). The read above already populated it.
        crate::utils::settings::settings_cache::reset_settings_cache();
        let approved = all_configured_mcp_servers_readonly(&global, &project);
        assert_eq!(approved["docs"].scope, ConfigScope::Project);

        crate::bootstrap::state::set_is_interactive(previous_interactive);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn parse_mcp_config_reports_missing_env_warning_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("COMETIX_MCP_PARSE_MISSING");
        let parsed = parse_mcp_config_value_readonly(
            &serde_json::json!({
                "mcpServers": {
                    "docs": {
                        "type": "http",
                        "url": "https://example.test/${COMETIX_MCP_PARSE_MISSING}"
                    }
                }
            }),
            true,
            ConfigScope::User,
            None,
        );

        assert_eq!(
            parsed.servers["docs"].transport,
            super::super::types::Transport::Http
        );
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            parsed.errors[0].mcp_error_metadata.severity,
            McpConfigErrorSeverity::Warning
        );
        assert_eq!(
            parsed.errors[0].mcp_error_metadata.server_name.as_deref(),
            Some("docs")
        );
        assert!(
            parsed.errors[0]
                .message
                .contains("Missing environment variables: COMETIX_MCP_PARSE_MISSING")
        );
    }

    #[test]
    fn parse_mcp_config_file_reports_invalid_json_fatal_like_official() {
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-invalid-json-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let path = temp_dir.join(".mcp.json");
        std::fs::write(&path, "{ invalid json").unwrap();

        let parsed = parse_mcp_config_from_file_path_readonly(&path, true, ConfigScope::Project);
        assert!(parsed.servers.is_empty());
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            parsed.errors[0].mcp_error_metadata.severity,
            McpConfigErrorSeverity::Fatal
        );
        assert_eq!(parsed.errors[0].message, "MCP config is not a valid JSON");
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn get_mcp_config_by_name_reads_user_snapshot_like_official_lookup() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-config-by-name-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        std::fs::write(
            temp_dir.join(".claude.json"),
            serde_json::json!({
                "mcpServers": {
                    "docs": {
                        "type": "http",
                        "url": "https://example.com/mcp"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let config = get_mcp_config_by_name_readonly("docs").expect("docs config");
        assert_eq!(config.scope, ConfigScope::User);
        assert_eq!(config.transport, super::super::types::Transport::Http);
        assert_eq!(config.url.as_deref(), Some("https://example.com/mcp"));
        assert!(get_mcp_config_by_name_readonly("missing").is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn get_mcp_config_by_name_matches_official_source_precedence() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "cometix-mcp-config-by-name-precedence-{}",
            uuid::Uuid::new_v4()
        ));
        let config_dir = temp_dir.join("config");
        let managed_dir = temp_dir.join("managed");
        let project_dir = temp_dir.join("project");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&managed_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed_dir);
        let _project_root = ProjectRootGuard::pin(&project_dir);

        let project_path_for_key = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.clone());
        let project_key =
            crate::utils::config::normalize_project_path(&project_path_for_key.to_string_lossy());
        std::fs::write(
            config_dir.join(".claude.json"),
            serde_json::json!({
                "mcpServers": {
                    "docs": { "type": "http", "url": "https://user.example/mcp" },
                    "user-only": { "type": "http", "url": "https://user-only.example/mcp" }
                },
                "projects": {
                    project_key: {
                        "mcpServers": {
                            "docs": { "command": "local-docs" },
                            "local-only": { "command": "local-only" }
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            project_dir.join(".mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "docs": { "type": "http", "url": "https://project.example/mcp" },
                    "project-only": { "type": "http", "url": "https://project-only.example/mcp" }
                }
            })
            .to_string(),
        )
        .unwrap();
        crate::utils::config::clear_global_config_cache_for_testing();

        let docs = get_mcp_config_by_name_readonly("docs").expect("docs");
        assert_eq!(docs.scope, ConfigScope::Local);
        assert_eq!(docs.command.as_deref(), Some("local-docs"));
        assert_eq!(
            get_mcp_config_by_name_readonly("project-only")
                .expect("project-only")
                .scope,
            ConfigScope::Project
        );
        assert_eq!(
            get_mcp_config_by_name_readonly("user-only")
                .expect("user-only")
                .scope,
            ConfigScope::User
        );

        std::fs::write(
            managed_dir.join("managed-mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "docs": { "command": "enterprise-docs" }
                }
            })
            .to_string(),
        )
        .unwrap();
        crate::utils::config::clear_global_config_cache_for_testing();
        let docs = get_mcp_config_by_name_readonly("docs").expect("enterprise docs");
        assert_eq!(docs.scope, ConfigScope::Enterprise);
        assert_eq!(docs.command.as_deref(), Some("enterprise-docs"));
        assert_eq!(
            get_mcp_config_by_name_readonly("user-only")
                .expect("user-only still reachable like official lookup")
                .scope,
            ConfigScope::User
        );

        drop((_config, _managed));
        crate::utils::config::clear_global_config_cache_for_testing();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn is_mcp_server_disabled_reads_official_project_disabled_list() {
        let project = ProjectConfig {
            disabled_mcp_servers: Some(vec!["docs".to_string()]),
            ..ProjectConfig::default()
        };

        assert!(is_mcp_server_disabled_in_project_config("docs", &project));
        assert!(!is_mcp_server_disabled_in_project_config(
            "github", &project
        ));
    }

    #[test]
    fn set_mcp_server_enabled_persists_disabled_membership_like_official_toggle() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-set-enabled-{}", uuid::Uuid::new_v4()));
        let project_dir = temp_dir.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        let _session_write = EnvVarGuard::set("SESSION_WRITE_ENABLED", "1");
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _project_root = ProjectRootGuard::pin(&project_dir);

        set_mcp_server_enabled("docs", false).unwrap();
        assert!(is_mcp_server_disabled("docs"));
        set_mcp_server_enabled("docs", true).unwrap();
        assert!(!is_mcp_server_disabled("docs"));

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(temp_dir.join(".claude.json")).unwrap())
                .unwrap();
        let current_project_dir = std::env::current_dir().unwrap();
        let project_key =
            crate::utils::config::normalize_project_path(&current_project_dir.to_string_lossy());
        let disabled = value
            .get("projects")
            .and_then(|projects| projects.get(&project_key))
            .and_then(|project| project.get("disabledMcpServers"))
            .and_then(|servers| servers.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            disabled.iter().all(|value| value.as_str() != Some("docs")),
            "docs should be removed after enabling: {disabled:?}"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    #[test]
    fn mcp_dedup_first_seen_order_matches_official_bun() {
        // services/mcp/config.ts:224-268: Object.entries + first signature wins.
        // Source oracle also checks numeric own keys and the retained env value.
        let cases: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-risk-fixes-0915/order-oracle.json"
        ))
        .unwrap();
        for case in cases.as_array().unwrap() {
            let entries = |values: &Value| -> indexmap::IndexMap<String, ScopedMcpServerConfig> {
                values
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| {
                        let name = v.as_str().unwrap();
                        let (key, mut config) =
                            test_scoped_config(name, Transport::Stdio, Some("echo"), &[], None);
                        config.env.insert("CHOSEN".into(), name.into());
                        (key, config)
                    })
                    .collect()
            };
            let result = dedup_plugin_mcp_servers_readonly(
                &entries(&case["plugin"]),
                &entries(&case["manual"]),
            );
            assert_eq!(
                serde_json::json!(result.servers.keys().collect::<Vec<_>>()),
                case["keys"],
                "{case}"
            );
            assert_eq!(
                serde_json::json!(
                    result
                        .servers
                        .values()
                        .map(|v| &v.env["CHOSEN"])
                        .collect::<Vec<_>>()
                ),
                case["chosen"],
                "{case}"
            );
            assert_eq!(
                serde_json::json!(
                    result
                        .suppressed
                        .iter()
                        .map(|v| serde_json::json!({"name":v.name,"duplicateOf":v.duplicate_of}))
                        .collect::<Vec<_>>()
                ),
                case["suppressed"],
                "{case}"
            );
        }
    }

    #[test]
    fn mcp_scope_parse_preserves_source_own_key_order_for_dedup() {
        // config.ts parseMcpConfig + dedupPluginMcpServers: parsing must not
        // alphabetize names before the source's first-seen dedup loop.
        let input: Value = serde_json::from_str(r#"{"mcpServers":{"z":{"command":"echo","env":{"CHOSEN":"z"}},"a":{"command":"echo","env":{"CHOSEN":"a"}}}}"#).unwrap();
        let parsed = parse_mcp_config_value_readonly(&input, false, ConfigScope::User, None);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(
            parsed
                .servers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        let result = dedup_plugin_mcp_servers_readonly(&parsed.servers, &indexmap::IndexMap::new());
        assert_eq!(
            result
                .servers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z"]
        );
        assert_eq!(result.servers["z"].env["CHOSEN"], "z");
    }

    #[test]
    fn dynamic_mcp_override_retains_official_property_position() {
        // main.tsx --mcp-config accumulation uses object spread: overriding
        // an existing key replaces the value without moving its position.
        let result = parse_dynamic_mcp_configs_readonly(&[
            r#"{"mcpServers":{"z":{"command":"echo"},"a":{"command":"echo"}}}"#.into(),
            r#"{"mcpServers":{"z":{"command":"printf"},"b":{"command":"echo"}}}"#.into(),
        ]);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result
                .servers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z", "a", "b"]
        );
        assert_eq!(result.servers["z"].command.as_deref(), Some("printf"));
    }
    #[test]
    fn merged_mcp_policy_filter_uses_official_numeric_own_key_order() {
        // config.ts#filterMcpServersByPolicy uses Object.entries after scopes
        // have merged, so an integer key appended by a later scope moves first.
        let mut configs = indexmap::IndexMap::from([
            test_scoped_config("9", Transport::Stdio, Some("nine"), &[], None),
            test_scoped_config("z", Transport::Stdio, Some("zed"), &[], None),
        ]);
        configs.extend([test_scoped_config(
            "2",
            Transport::Stdio,
            Some("two"),
            &[],
            None,
        )]);
        let result = filter_mcp_servers_by_policy_with_settings(
            configs,
            &SettingsJson::default(),
            &SettingsJson::default(),
        );
        assert!(result.blocked.is_empty());
        assert_eq!(
            result
                .allowed
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["2", "9", "z"]
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_mcp_returns_loading_errors_then_duplicate_notices() {
        // config.ts:1146-1163 loads plugins concurrently and logs MCP errors;
        // :1218-1230 appends suppression notices afterwards and returns both.
        struct InlinePluginsGuard(Vec<PathBuf>);
        impl Drop for InlinePluginsGuard {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(std::mem::take(&mut self.0));
                crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let root = std::env::temp_dir().join(format!(
            "cometix-mcp-startup-errors-{}",
            uuid::Uuid::new_v4()
        ));
        let config = root.join("config");
        let managed = root.join("managed");
        let plugin = root.join("plugin");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo","mcpServers":"./missing.mcpb"}"#,
        )
        .unwrap();
        std::fs::write(
            plugin.join(".mcp.json"),
            r#"{"duplicate":{"command":"echo","args":["same"]}}"#,
        )
        .unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let _project = ProjectRootGuard::pin(&root);
        let _inline = InlinePluginsGuard(crate::bootstrap::state::get_inline_plugins());
        crate::bootstrap::state::set_inline_plugins(vec![plugin]);
        crate::utils::plugins::plugin_loader::clear_plugin_cache(None);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let global = GlobalConfig {
            mcp_servers: Some(serde_json::json!({"manual":{"command":"echo","args":["same"]}})),
            ..Default::default()
        };
        let loaded = crate::utils::plugins::plugin_loader::load_all_plugins_cache_only_from_sync();
        assert_eq!(
            loaded
                .enabled
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["demo"],
            "{:?}",
            loaded.errors
        );
        let result = get_claude_code_mcp_configs_readonly(
            &global,
            &ProjectConfig::default(),
            &indexmap::IndexMap::new(),
            &indexmap::IndexMap::new(),
        );
        assert!(result.servers.contains_key("manual"));
        assert!(!result.servers.contains_key("plugin:demo:duplicate"));
        let projected: Vec<Value> = result
            .errors
            .iter()
            .map(|error| {
                let full = serde_json::to_value(error).unwrap();
                if matches!(
                    error,
                    crate::types::plugin::PluginError::McpServerSuppressedDuplicate { .. }
                ) {
                    full
                } else {
                    serde_json::json!({"type":full["type"],"plugin":full["plugin"]})
                }
            })
            .collect();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-lifecycle-0915/startup-errors-oracle.json"
        ))
        .unwrap();
        assert_eq!(serde_json::json!(projected), oracle);
        std::fs::remove_dir_all(root).unwrap();
    }
}
