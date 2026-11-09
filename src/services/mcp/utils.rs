//! Maps to: CC `services/mcp/utils.ts` read-only helpers used by MCP UI.
//!
//! This slice ports read-only path/label/string/config-projection helpers. It
//! does not start clients, mutate app state, or perform network/auth work.

use super::mcp_string_utils::mcp_info_from_string;
use super::normalization::normalize_name_for_mcp;
use super::types::McpServerConnection;
use super::types::{ConfigScope, ScopedMcpServerConfig, ServerResource, Transport};
use crate::commands::Command;
use crate::state::app_state_store::McpState;
use crate::types::tools::Tool;
use crate::utils::settings::types::SettingsJson;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Maps to: CC `services/mcp/utils.ts#describeMcpConfigFilePath`.
pub fn describe_mcp_config_file_path(scope: ConfigScope) -> String {
    match scope {
        ConfigScope::User => crate::utils::env::get_global_state_file()
            .display()
            .to_string(),
        ConfigScope::Project => std::env::current_dir()
            .unwrap_or_default()
            .join(".mcp.json")
            .display()
            .to_string(),
        ConfigScope::Local => format!(
            "{} [project: {}]",
            crate::utils::env::get_global_state_file().display(),
            std::env::current_dir().unwrap_or_default().display()
        ),
        ConfigScope::Dynamic => "Dynamically configured".to_string(),
    }
}

/// Maps to: CC `services/mcp/utils.ts#getScopeLabel`.
pub fn get_scope_label(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Local => "Local config (private to you in this project)",
        ConfigScope::Project => "Project config (shared via .mcp.json)",
        ConfigScope::User => "User config (available in all your projects)",
        ConfigScope::Dynamic => "Dynamic config (from command line)",
    }
}

const CONFIG_SCOPE_OPTIONS: &[&str] = &["local", "user", "project", "dynamic"];

fn config_scope_from_str(scope: &str) -> Option<ConfigScope> {
    match scope {
        "local" => Some(ConfigScope::Local),
        "user" => Some(ConfigScope::User),
        "project" => Some(ConfigScope::Project),
        "dynamic" => Some(ConfigScope::Dynamic),
        _ => None,
    }
}

/// Maps to: CC `services/mcp/utils.ts#ensureConfigScope`.
pub fn ensure_config_scope(scope: Option<&str>) -> anyhow::Result<ConfigScope> {
    let Some(scope) = scope.filter(|scope| !scope.is_empty()) else {
        return Ok(ConfigScope::Local);
    };
    config_scope_from_str(scope).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid scope: {scope}. Must be one of: {}",
            CONFIG_SCOPE_OPTIONS.join(", ")
        )
    })
}

/// Maps to: CC `services/mcp/utils.ts#ensureTransport`.
pub fn ensure_transport(transport: Option<&str>) -> anyhow::Result<Transport> {
    match transport.filter(|transport| !transport.is_empty()) {
        None => Ok(Transport::Stdio),
        Some("stdio") => Ok(Transport::Stdio),
        Some("sse") => Ok(Transport::Sse),
        Some("http") => Ok(Transport::Http),
        Some(transport) => Err(anyhow::anyhow!(
            "Invalid transport type: {transport}. Must be one of: stdio, sse, http"
        )),
    }
}

/// Maps to: CC `services/mcp/utils.ts#parseHeaders`.
pub fn parse_headers(header_array: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for header in header_array {
        let Some(colon_index) = header.find(':') else {
            return Err(anyhow::anyhow!(
                "Invalid header format: \"{header}\". Expected format: \"Header-Name: value\""
            ));
        };
        let key = header[..colon_index].trim();
        let value = header[colon_index + 1..].trim();
        if key.is_empty() {
            return Err(anyhow::anyhow!(
                "Invalid header: \"{header}\". Header name cannot be empty."
            ));
        }
        headers.insert(key.to_string(), value.to_string());
    }
    Ok(headers)
}

/// Maps to: CC `services/mcp/utils.ts#filterToolsByServer`.
pub fn filter_tools_by_server(tools: &[Tool], server_name: &str) -> Vec<Tool> {
    let prefix = format!("mcp__{}__", normalize_name_for_mcp(server_name));
    tools
        .iter()
        .filter(|tool| tool.name.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#excludeToolsByServer`.
pub fn exclude_tools_by_server(tools: &[Tool], server_name: &str) -> Vec<Tool> {
    let prefix = format!("mcp__{}__", normalize_name_for_mcp(server_name));
    tools
        .iter()
        .filter(|tool| !tool.name.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#commandBelongsToServer`.
pub fn command_belongs_to_server(command_name: &str, server_name: &str) -> bool {
    let normalized = normalize_name_for_mcp(server_name);
    command_name.starts_with(&format!("mcp__{normalized}__"))
        || command_name.starts_with(&format!("{normalized}:"))
}

/// Maps to: CC `services/mcp/utils.ts#filterCommandsByServer`.
pub fn filter_commands_by_server(commands: &[Command], server_name: &str) -> Vec<Command> {
    commands
        .iter()
        .filter(|command| command_belongs_to_server(&command.name, server_name))
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts:85-94#filterMcpPromptsByServer`: the
/// server's commands that are not MCP skills (`loadedFrom === 'mcp'`).
pub fn filter_mcp_prompts_by_server(commands: &[Command], server_name: &str) -> Vec<Command> {
    commands
        .iter()
        .filter(|command| {
            command_belongs_to_server(&command.name, server_name)
                && !(command.kind == crate::commands::CommandKind::Prompt
                    && command.loaded_from
                        == Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp))
        })
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#filterResourcesByServer`.
pub fn filter_resources_by_server(
    resources: &[ServerResource],
    server_name: &str,
) -> Vec<ServerResource> {
    resources
        .iter()
        .filter(|resource| resource.server == server_name)
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#excludeCommandsByServer`.
pub fn exclude_commands_by_server(commands: &[Command], server_name: &str) -> Vec<Command> {
    commands
        .iter()
        .filter(|command| !command_belongs_to_server(&command.name, server_name))
        .cloned()
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#excludeResourcesByServer`.
pub fn exclude_resources_by_server(
    resources: &BTreeMap<String, Vec<ServerResource>>,
    server_name: &str,
) -> BTreeMap<String, Vec<ServerResource>> {
    let mut result = resources.clone();
    result.remove(server_name);
    result
}

/// Maps to: CC `services/mcp/utils.ts#isToolFromMcpServer`.
pub fn is_tool_from_mcp_server(tool_name: &str, server_name: &str) -> bool {
    mcp_info_from_string(tool_name).is_some_and(|info| info.server_name == server_name)
}

/// Maps to: CC `services/mcp/utils.ts#isMcpTool` for name-only callers.
pub fn is_mcp_tool_name(tool_name: &str) -> bool {
    tool_name.starts_with("mcp__")
}

/// Maps to: CC `services/mcp/utils.ts#isMcpTool`.
pub fn is_mcp_tool(tool: &Tool) -> bool {
    is_mcp_tool_name(&tool.name) || tool.is_mcp
}

/// Maps to: CC `services/mcp/utils.ts:254-256#isMcpCommand`; `isMcp` is
/// the MCP command source here.
pub fn is_mcp_command(command: &Command) -> bool {
    command.name.starts_with("mcp__") || command.source == crate::commands::CommandSource::Mcp
}

/// Maps to: CC `services/mcp/utils.ts#getMcpServerScopeFromToolName`.
pub fn get_mcp_server_scope_from_tool_name(tool_name: &str) -> Option<ConfigScope> {
    if !is_mcp_tool_name(tool_name) {
        return None;
    }
    let info = mcp_info_from_string(tool_name)?;
    super::config::get_mcp_config_by_name_readonly(&info.server_name).map(|config| config.scope)
}

fn strip_url_query_like_js_url_search_clear(url: &str) -> String {
    let Some(query_index) = url.find('?') else {
        return url.to_string();
    };
    if let Some(hash_index) = url[query_index + 1..].find('#') {
        let hash_start = query_index + 1 + hash_index;
        format!("{}{}", &url[..query_index], &url[hash_start..])
    } else {
        url[..query_index].to_string()
    }
}

/// Maps to: CC `services/mcp/utils.ts#getLoggingSafeMcpBaseUrl`.
pub fn get_logging_safe_mcp_base_url(config: &ScopedMcpServerConfig) -> Option<String> {
    let url = config.url.as_ref()?;
    let scheme_separator = url.find("://")?;
    if scheme_separator == 0 || url.chars().any(char::is_whitespace) {
        return None;
    }
    let after_scheme = &url[scheme_separator + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    if authority_end == 0 {
        return None;
    }

    let mut safe = strip_url_query_like_js_url_search_clear(url);
    if safe.ends_with('/') {
        safe.pop();
    }
    Some(safe)
}

fn sorted_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sorted = Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(value) = object.get(key) {
                    sorted.insert(key.clone(), sorted_json_value(value));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(sorted_json_value).collect()),
        _ => value.clone(),
    }
}

fn string_map_value(values: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (key, value) in values {
        object.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(object)
}

fn scoped_mcp_config_hash_payload(config: &ScopedMcpServerConfig) -> Value {
    let mut object = Map::new();
    object.insert(
        "type".to_string(),
        Value::String(config.transport.as_str().to_string()),
    );
    if let Some(command) = &config.command {
        object.insert("command".to_string(), Value::String(command.clone()));
    }
    if config.transport == Transport::Stdio || !config.args.is_empty() {
        object.insert(
            "args".to_string(),
            Value::Array(config.args.iter().cloned().map(Value::String).collect()),
        );
    }
    if !config.env.is_empty() {
        object.insert("env".to_string(), string_map_value(&config.env));
    }
    if let Some(url) = &config.url {
        object.insert("url".to_string(), Value::String(url.clone()));
    }
    if !config.headers.is_empty() {
        object.insert("headers".to_string(), string_map_value(&config.headers));
    }
    if let Some(headers_helper) = &config.headers_helper {
        object.insert(
            "headersHelper".to_string(),
            Value::String(headers_helper.clone()),
        );
    }
    if let Some(oauth) = &config.oauth {
        object.insert("oauth".to_string(), sorted_json_value(oauth));
    }
    if let Some(ide_name) = &config.ide_name {
        object.insert("ideName".to_string(), Value::String(ide_name.clone()));
    }
    if let Some(ide_running_in_windows) = config.ide_running_in_windows {
        object.insert(
            "ideRunningInWindows".to_string(),
            Value::Bool(ide_running_in_windows),
        );
    }
    if let Some(name) = &config.name {
        object.insert("name".to_string(), Value::String(name.clone()));
    }
    if let Some(auth_token) = &config.auth_token {
        object.insert("authToken".to_string(), Value::String(auth_token.clone()));
    }
    let value = Value::Object(object);
    sorted_json_value(&value)
}

/// Maps to: CC `services/mcp/utils.ts#hashMcpConfig`.
pub fn hash_mcp_config(config: &ScopedMcpServerConfig) -> String {
    let payload = scoped_mcp_config_hash_payload(config);
    let stable = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    let digest = Sha256::digest(stable.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(16)
        .collect()
}

/// Maps to: CC `services/mcp/utils.ts#excludeStalePluginClients` return value.
#[derive(Clone, Debug, PartialEq)]
pub struct ExcludeStalePluginClientsResult {
    pub state: McpState,
    /// Stale clients are returned so the caller can perform service-boundary
    /// cleanup (`clearServerCache`) outside AppState reconciliation, matching CC.
    pub stale: Vec<McpServerConnection>,
}

fn is_stale_plugin_client(
    server: &McpServerConnection,
    configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> bool {
    match configs.get(server.name()) {
        Some(fresh) => hash_mcp_config(server.config()) != hash_mcp_config(fresh),
        None => server.config().scope == ConfigScope::Dynamic,
    }
}

/// Maps to: CC `services/mcp/utils.ts#excludeStalePluginClients`.
pub fn exclude_stale_plugin_clients(
    state: &McpState,
    configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
) -> ExcludeStalePluginClientsResult {
    let stale = state
        .clients
        .iter()
        .filter(|server| is_stale_plugin_client(server, configs))
        .cloned()
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return ExcludeStalePluginClientsResult {
            state: state.clone(),
            stale,
        };
    }

    let stale_names = stale
        .iter()
        .map(|server| server.name())
        .collect::<std::collections::BTreeSet<_>>();
    let mut next = state.clone();
    // CC `excludeToolsByServer`: by the server's `mcp__<name>__` prefix.
    let stale_prefixes = stale
        .iter()
        .map(|server| format!("mcp__{}__", normalize_name_for_mcp(server.name())))
        .collect::<Vec<_>>();
    next.tools.retain(|tool| {
        !stale_prefixes
            .iter()
            .any(|prefix| tool.name.starts_with(prefix))
    });
    next.commands.retain(|command| {
        !stale
            .iter()
            .any(|server| command_belongs_to_server(command.name.as_ref(), server.name()))
    });
    for name in &stale_names {
        next.resources.remove(*name);
    }
    next.clients
        .retain(|server| !stale_names.contains(server.name()));
    ExcludeStalePluginClientsResult { state: next, stale }
}

/// Maps to: CC `services/mcp/utils.ts#getProjectMcpServerStatus` return type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMcpServerStatus {
    Approved,
    Rejected,
    Pending,
}

impl ProjectMcpServerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Pending => "pending",
        }
    }
}

/// Maps to: CC `services/mcp/utils.ts#getProjectMcpServerStatus`.
pub fn get_project_mcp_server_status_from_settings(
    server_name: &str,
    settings: &SettingsJson,
    project_settings_enabled: bool,
    has_skip_dangerous_mode_permission_prompt: bool,
    is_non_interactive_session: bool,
) -> ProjectMcpServerStatus {
    let normalized_name = normalize_name_for_mcp(server_name);

    if settings
        .disabled_mcpjson_servers
        .as_ref()
        .is_some_and(|servers| {
            servers
                .iter()
                .any(|name| normalize_name_for_mcp(name) == normalized_name)
        })
    {
        return ProjectMcpServerStatus::Rejected;
    }

    if settings
        .enabled_mcpjson_servers
        .as_ref()
        .is_some_and(|servers| {
            servers
                .iter()
                .any(|name| normalize_name_for_mcp(name) == normalized_name)
        })
        || settings.enable_all_project_mcp_servers == Some(true)
    {
        return ProjectMcpServerStatus::Approved;
    }

    if has_skip_dangerous_mode_permission_prompt && project_settings_enabled {
        return ProjectMcpServerStatus::Approved;
    }

    if is_non_interactive_session && project_settings_enabled {
        return ProjectMcpServerStatus::Approved;
    }

    ProjectMcpServerStatus::Pending
}

/// Maps to: CC `services/mcp/utils.ts#getProjectMcpServerStatus`.
pub fn get_project_mcp_server_status(server_name: &str) -> ProjectMcpServerStatus {
    get_project_mcp_server_status_from_settings(
        server_name,
        &crate::utils::settings::get_initial_settings(),
        crate::utils::settings::is_setting_source_enabled(
            crate::utils::settings::SettingSource::Project,
        ),
        crate::utils::settings::has_skip_dangerous_mode_permission_prompt(),
        crate::bootstrap::state::get_is_non_interactive_session(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_mcp_config_file_path_matches_official_scope_shapes() {
        assert!(describe_mcp_config_file_path(ConfigScope::User).ends_with("state.json"));
        assert!(describe_mcp_config_file_path(ConfigScope::Project).ends_with(".mcp.json"));
        assert!(describe_mcp_config_file_path(ConfigScope::Local).contains("[project:"));
        assert_eq!(
            describe_mcp_config_file_path(ConfigScope::Dynamic),
            "Dynamically configured"
        );
    }

    #[test]
    fn get_scope_label_matches_official_copy() {
        assert_eq!(
            get_scope_label(ConfigScope::Project),
            "Project config (shared via .mcp.json)"
        );
    }

    #[test]
    fn ensure_config_scope_transport_and_headers_match_official_cli_helpers() {
        assert_eq!(ensure_config_scope(None).unwrap(), ConfigScope::Local);
        assert_eq!(ensure_config_scope(Some("")).unwrap(), ConfigScope::Local);
        assert!(ensure_config_scope(Some("claudeai")).is_err());
        assert!(ensure_config_scope(Some("enterprise")).is_err());
        assert_eq!(
            ensure_config_scope(Some("workspace"))
                .unwrap_err()
                .to_string(),
            "Invalid scope: workspace. Must be one of: local, user, project, dynamic"
        );

        assert_eq!(ensure_transport(None).unwrap(), Transport::Stdio);
        assert_eq!(ensure_transport(Some("sse")).unwrap(), Transport::Sse);
        assert_eq!(ensure_transport(Some("http")).unwrap(), Transport::Http);
        assert_eq!(
            ensure_transport(Some("ws")).unwrap_err().to_string(),
            "Invalid transport type: ws. Must be one of: stdio, sse, http"
        );

        let headers = parse_headers(&[
            "Authorization: Bearer token".to_string(),
            "X-Test:   value with spaces  ".to_string(),
        ])
        .unwrap();
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(
            headers.get("X-Test").map(String::as_str),
            Some("value with spaces")
        );
        assert_eq!(
            parse_headers(&["MissingColon".to_string()])
                .unwrap_err()
                .to_string(),
            "Invalid header format: \"MissingColon\". Expected format: \"Header-Name: value\""
        );
        assert_eq!(
            parse_headers(&[": value".to_string()])
                .unwrap_err()
                .to_string(),
            "Invalid header: \": value\". Header name cannot be empty."
        );
    }

    #[test]
    fn mcp_tool_filter_scope_and_logging_url_helpers_match_official_utils() {
        let tools = vec![
            Tool {
                name: "mcp__docs_server__search".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                ..Default::default()
            },
            Tool {
                name: "mcp__other__lookup".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                ..Default::default()
            },
            Tool {
                name: "PlainTool".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                is_mcp: true,
                ..Default::default()
            },
        ];

        assert_eq!(
            filter_tools_by_server(&tools, "docs server")
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["mcp__docs_server__search"]
        );
        assert_eq!(exclude_tools_by_server(&tools, "other").len(), 2);
        assert!(is_tool_from_mcp_server(
            "mcp__docs_server__tool__part",
            "docs_server"
        ));
        assert!(is_mcp_tool_name("mcp__docs__search"));
        assert!(is_mcp_tool(&tools[2]));

        let commands = vec![
            mcp_prompt_command("mcp__docs_server__summarize"),
            mcp_prompt_command("docs_server:skill"),
            mcp_prompt_command("mcp__other__summarize"),
        ];
        assert!(command_belongs_to_server(
            "mcp__docs_server__summarize",
            "docs server"
        ));
        assert!(command_belongs_to_server(
            "docs_server:skill",
            "docs server"
        ));
        assert_eq!(
            filter_commands_by_server(&commands, "docs server")
                .iter()
                .map(|command| command.name.as_ref())
                .collect::<Vec<_>>(),
            vec!["mcp__docs_server__summarize", "docs_server:skill"]
        );
        assert_eq!(
            filter_mcp_prompts_by_server(&commands, "other")
                .iter()
                .map(|command| command.name.as_ref())
                .collect::<Vec<_>>(),
            vec!["mcp__other__summarize"]
        );
        // CC `utils.ts:89-93`: an MCP skill (`loadedFrom === 'mcp'`) is not
        // one of the server's prompts.
        let mut skill = mcp_prompt_command("mcp__other__skill");
        skill.loaded_from = Some(crate::skills::load_skills_dir::SkillLoadedFrom::Mcp);
        assert_eq!(
            filter_mcp_prompts_by_server(&[commands[2].clone(), skill], "other").len(),
            1
        );
        assert_eq!(
            exclude_commands_by_server(&commands, "docs server").len(),
            1
        );
        assert!(is_mcp_command(&commands[0]));

        let resources = vec![
            server_resource("docs", "file://a"),
            server_resource("memory", "mem://note"),
        ];
        assert_eq!(
            filter_resources_by_server(&resources, "docs")
                .iter()
                .map(|resource| resource.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["file://a"]
        );
        let resource_map = BTreeMap::from([
            ("docs".to_string(), vec![resources[0].clone()]),
            ("memory".to_string(), vec![resources[1].clone()]),
        ]);
        let excluded = exclude_resources_by_server(&resource_map, "docs");
        assert!(!excluded.contains_key("docs"));
        assert!(excluded.contains_key("memory"));

        assert_eq!(get_mcp_server_scope_from_tool_name("Write"), None);

        let mut config = scoped_config(Transport::Http);
        config.url = Some("https://example.test/mcp/?token=secret#frag".to_string());
        assert_eq!(
            get_logging_safe_mcp_base_url(&config).as_deref(),
            Some("https://example.test/mcp/#frag")
        );
        config.url = Some("https://example.test/mcp/?token=secret".to_string());
        assert_eq!(
            get_logging_safe_mcp_base_url(&config).as_deref(),
            Some("https://example.test/mcp")
        );
        config.url = Some("not a url".to_string());
        assert_eq!(get_logging_safe_mcp_base_url(&config), None);
        config.transport = Transport::Stdio;
        config.url = None;
        assert_eq!(get_logging_safe_mcp_base_url(&config), None);
    }

    #[test]
    fn hash_mcp_config_ignores_scope_and_sorts_nested_objects() {
        let mut first = scoped_config(Transport::Http);
        first.scope = ConfigScope::User;
        first.headers.insert("z".to_string(), "last".to_string());
        first.headers.insert("a".to_string(), "first".to_string());
        first.oauth = Some(serde_json::json!({
            "clientId": "abc",
            "xaa": true,
            "nested": { "b": 2, "a": 1 }
        }));

        let mut second = first.clone();
        second.scope = ConfigScope::Project;
        second.headers.clear();
        second.headers.insert("a".to_string(), "first".to_string());
        second.headers.insert("z".to_string(), "last".to_string());
        second.oauth = Some(serde_json::json!({
            "nested": { "a": 1, "b": 2 },
            "xaa": true,
            "clientId": "abc"
        }));

        assert_eq!(hash_mcp_config(&first), hash_mcp_config(&second));
        second.url = Some("https://example.test/changed".to_string());
        assert_ne!(hash_mcp_config(&first), hash_mcp_config(&second));
    }

    #[test]
    fn exclude_stale_plugin_clients_matches_official_dynamic_and_hash_rules() {
        let mut dynamic_config = scoped_config(Transport::Stdio);
        dynamic_config.scope = ConfigScope::Dynamic;
        let mut changed_config = scoped_config(Transport::Http);
        changed_config.scope = ConfigScope::User;
        let mut kept_config = scoped_config(Transport::Http);
        kept_config.scope = ConfigScope::User;
        kept_config.url = Some("https://example.test/kept".to_string());

        let state = McpState {
            clients: vec![
                crate::services::mcp::client::McpConnectionDiscovery::pending(
                    "removed-plugin",
                    &dynamic_config,
                )
                .server,
                crate::services::mcp::client::McpConnectionDiscovery::pending(
                    "changed-user",
                    &changed_config,
                )
                .server,
                crate::services::mcp::client::McpConnectionDiscovery::pending(
                    "kept-user",
                    &kept_config,
                )
                .server,
            ],
            ..McpState::default()
        };

        let mut fresh_changed = changed_config.clone();
        fresh_changed.url = Some("https://example.test/changed".to_string());
        let configs = indexmap::IndexMap::from([
            ("changed-user".to_string(), fresh_changed),
            ("kept-user".to_string(), kept_config),
        ]);

        let result = exclude_stale_plugin_clients(&state, &configs);
        assert_eq!(
            result
                .stale
                .iter()
                .map(|server| server.name())
                .collect::<Vec<_>>(),
            vec!["removed-plugin", "changed-user"]
        );
        assert_eq!(result.state.clients.len(), 1);
        assert_eq!(result.state.clients[0].name(), "kept-user");
    }

    #[test]
    fn project_mcp_server_status_matches_official_settings_rules() {
        let settings = SettingsJson {
            disabled_mcpjson_servers: Some(vec!["Blocked Server".to_string()]),
            enabled_mcpjson_servers: Some(vec!["Docs Server".to_string()]),
            ..SettingsJson::default()
        };

        assert_eq!(
            get_project_mcp_server_status_from_settings(
                "Blocked_Server",
                &settings,
                true,
                false,
                false,
            ),
            ProjectMcpServerStatus::Rejected
        );
        assert_eq!(
            get_project_mcp_server_status_from_settings(
                "Docs_Server",
                &settings,
                true,
                false,
                false,
            ),
            ProjectMcpServerStatus::Approved
        );
        assert_eq!(
            get_project_mcp_server_status_from_settings("other", &settings, true, false, false),
            ProjectMcpServerStatus::Pending
        );
        assert_eq!(
            get_project_mcp_server_status_from_settings("other", &settings, true, true, false),
            ProjectMcpServerStatus::Approved
        );
        assert_eq!(
            get_project_mcp_server_status_from_settings("other", &settings, true, false, true),
            ProjectMcpServerStatus::Approved
        );
        assert_eq!(ProjectMcpServerStatus::Pending.as_str(), "pending");
    }

    /// An MCP prompt command renamed to `name`, so a test can name it outside
    /// the `mcp__<server>__<prompt>` form.
    fn mcp_prompt_command(name: &str) -> Command {
        let mut command =
            crate::services::mcp::client::mcp_command_for_test("server", "prompt", None, &[]);
        command.name = name.to_string().into();
        command.user_facing_name = Some(name.to_string().into());
        command
    }

    fn server_resource(server: &str, uri: &str) -> ServerResource {
        ServerResource {
            server: server.to_string(),
            uri: uri.to_string(),
            name: uri.to_string(),
            description: None,
            mime_type: None,
        }
    }

    fn scoped_config(transport: Transport) -> crate::services::mcp::types::ScopedMcpServerConfig {
        crate::services::mcp::types::ScopedMcpServerConfig {
            name: None,
            scope: ConfigScope::Dynamic,
            transport,
            command: (transport == Transport::Stdio).then(|| "agent-mcp".to_string()),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: matches!(transport, Transport::Sse | Transport::Http | Transport::Ws)
                .then(|| "https://example.test/mcp".to_string()),
            headers: std::collections::BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            plugin_source: None,
        }
    }
}
