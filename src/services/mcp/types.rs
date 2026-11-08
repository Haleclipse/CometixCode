//! Maps to: CC `services/mcp/types.ts`.

use crate::utils::zod::{self, Schema};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Maps to: CC `ConfigScope`. @nyan: no `enterprise`/`managed` scopes (no
/// managed policy layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigScope {
    Local,
    User,
    Project,
    Dynamic,
    ClaudeAi,
}

impl ConfigScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::User => "user",
            Self::Project => "project",
            Self::Dynamic => "dynamic",
            Self::ClaudeAi => "claudeai",
        }
    }
}

/// Maps to: CC `Transport` plus the `claudeai-proxy` config discriminator used
/// by `MCPSettings.tsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Sse,
    SseIde,
    Http,
    Ws,
    WsIde,
    Sdk,
    ClaudeAiProxy,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
            Self::SseIde => "sse-ide",
            Self::Http => "http",
            Self::Ws => "ws",
            Self::WsIde => "ws-ide",
            Self::Sdk => "sdk",
            Self::ClaudeAiProxy => "claudeai-proxy",
        }
    }
}

/// Maps to: CC `McpServerConfig` transport discriminator.
pub fn transport_from_config(config: &crate::utils::config::McpServerConfig) -> Transport {
    match config.server_type.as_deref() {
        Some("stdio") => Transport::Stdio,
        Some("sse") => Transport::Sse,
        Some("sse-ide") => Transport::SseIde,
        Some("http") => Transport::Http,
        Some("ws") => Transport::Ws,
        Some("ws-ide") => Transport::WsIde,
        Some("sdk") => Transport::Sdk,
        Some("claudeai-proxy") => Transport::ClaudeAiProxy,
        // Older configs inferred remote streamable HTTP from URL presence.
        None if config.url.is_some() => Transport::Http,
        None => Transport::Stdio,
        Some(_) => Transport::Stdio,
    }
}

/// Maps to: CC `ScopedMcpServerConfig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedMcpServerConfig {
    pub scope: ConfigScope,
    pub transport: Transport,
    /// Maps to: CC `McpSdkServerConfigSchema.name`, preserved by the intersection.
    pub name: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    pub oauth: Option<serde_json::Value>,
    pub ide_name: Option<String>,
    /// Maps to: CC `McpSSEIDEServerConfigSchema` / `McpWebSocketIDEServerConfigSchema`.
    pub ide_running_in_windows: Option<bool>,
    pub auth_token: Option<String>,
    pub id: Option<String>,
    /// Maps to: CC `ScopedMcpServerConfig.pluginSource`.
    pub plugin_source: Option<String>,
}

impl ScopedMcpServerConfig {
    pub fn from_config(scope: ConfigScope, config: &crate::utils::config::McpServerConfig) -> Self {
        Self {
            scope,
            transport: transport_from_config(config),
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone().unwrap_or_default(),
            env: config.env.clone().unwrap_or_default().into_iter().collect(),
            url: config.url.clone(),
            headers: config
                .headers
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            headers_helper: config.headers_helper.clone(),
            oauth: config.oauth.clone(),
            ide_name: config.ide_name.clone(),
            ide_running_in_windows: config.ide_running_in_windows,
            auth_token: config.auth_token.clone(),
            id: config.id.clone(),
            plugin_source: None,
        }
    }
}

/// Maps to: CC `services/mcp/types.ts#ServerResource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerResource {
    pub server: String,
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

/// Maps to: CC `MCPServerConnection['type']` — the discriminant of the
/// `services/mcp/types.ts:221` union, as a value of its own for the places
/// that compare or print it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpServerConnectionType {
    Connected,
    Failed,
    NeedsAuth,
    Pending,
    Disabled,
}

impl McpServerConnectionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Failed => "failed",
            Self::NeedsAuth => "needs-auth",
            Self::Pending => "pending",
            Self::Disabled => "disabled",
        }
    }
}

/// Maps to: CC `MCPServerConnection` (`services/mcp/types.ts:221-226`), as
/// stored in `AppState.mcp.clients` (`state/AppStateStore.ts:174`).
///
/// Like CC's, a record carries no tools, prompts or resources: those live in
/// `AppState.mcp.tools` / `.commands` / `.resources`, and views pick a
/// server's share out of them (`filterToolsByServer`,
/// `commandBelongsToServer`, `resources[name]`).
///
/// Each variant holds only its own fields. CC's transitions spread the
/// previous record (`{ ...client, type: 'failed' }`), so its non-connected
/// objects can still carry a `client`; no consumer reads one without first
/// checking `type === 'connected'`.
#[derive(Clone, Debug, PartialEq)]
pub enum McpServerConnection {
    Connected(ConnectedMcpServer),
    Failed(FailedMcpServer),
    NeedsAuth(NeedsAuthMcpServer),
    Pending(PendingMcpServer),
    Disabled(DisabledMcpServer),
}

/// Maps to: CC `ConnectedMCPServer` (`services/mcp/types.ts:180-192`). Its
/// `cleanup()` is `cleanup_connection(&server.client)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectedMcpServer {
    pub client: super::client::McpClient,
    pub name: String,
    pub capabilities: rmcp::model::ServerCapabilities,
    /// CC `serverInfo`: `client.getServerVersion()`, the server's whole
    /// `Implementation`.
    pub server_info: Option<rmcp::model::Implementation>,
    pub instructions: Option<String>,
    pub config: ScopedMcpServerConfig,
}

/// Maps to: CC `FailedMCPServer` (`services/mcp/types.ts:194-199`).
#[derive(Clone, Debug, PartialEq)]
pub struct FailedMcpServer {
    pub name: String,
    pub config: ScopedMcpServerConfig,
    pub error: Option<String>,
}

/// Maps to: CC `NeedsAuthMCPServer` (`services/mcp/types.ts:201-205`).
#[derive(Clone, Debug, PartialEq)]
pub struct NeedsAuthMcpServer {
    pub name: String,
    pub config: ScopedMcpServerConfig,
}

/// Maps to: CC `PendingMCPServer` (`services/mcp/types.ts:207-213`).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingMcpServer {
    pub name: String,
    pub config: ScopedMcpServerConfig,
    pub reconnect_attempt: Option<u32>,
    pub max_reconnect_attempts: Option<u32>,
}

/// Maps to: CC `DisabledMCPServer` (`services/mcp/types.ts:215-219`).
#[derive(Clone, Debug, PartialEq)]
pub struct DisabledMcpServer {
    pub name: String,
    pub config: ScopedMcpServerConfig,
}

impl McpServerConnection {
    /// CC `client.name`.
    pub fn name(&self) -> &str {
        match self {
            Self::Connected(server) => &server.name,
            Self::Failed(server) => &server.name,
            Self::NeedsAuth(server) => &server.name,
            Self::Pending(server) => &server.name,
            Self::Disabled(server) => &server.name,
        }
    }

    /// CC `client.type`.
    pub fn connection_type(&self) -> McpServerConnectionType {
        match self {
            Self::Connected(_) => McpServerConnectionType::Connected,
            Self::Failed(_) => McpServerConnectionType::Failed,
            Self::NeedsAuth(_) => McpServerConnectionType::NeedsAuth,
            Self::Pending(_) => McpServerConnectionType::Pending,
            Self::Disabled(_) => McpServerConnectionType::Disabled,
        }
    }

    /// CC `client.config`.
    pub fn config(&self) -> &ScopedMcpServerConfig {
        match self {
            Self::Connected(server) => &server.config,
            Self::Failed(server) => &server.config,
            Self::NeedsAuth(server) => &server.config,
            Self::Pending(server) => &server.config,
            Self::Disabled(server) => &server.config,
        }
    }

    /// The record when it is `type === 'connected'`.
    pub fn as_connected(&self) -> Option<&ConnectedMcpServer> {
        match self {
            Self::Connected(server) => Some(server),
            _ => None,
        }
    }

    /// `{ name, type: 'failed', config, error }`.
    pub fn failed(
        name: impl Into<String>,
        config: ScopedMcpServerConfig,
        error: Option<String>,
    ) -> Self {
        Self::Failed(FailedMcpServer {
            name: name.into(),
            config,
            error,
        })
    }

    /// `{ name, type: 'needs-auth', config }`.
    pub fn needs_auth(name: impl Into<String>, config: ScopedMcpServerConfig) -> Self {
        Self::NeedsAuth(NeedsAuthMcpServer {
            name: name.into(),
            config,
        })
    }

    /// `{ name, type: 'pending', config }`.
    pub fn pending(name: impl Into<String>, config: ScopedMcpServerConfig) -> Self {
        Self::Pending(PendingMcpServer {
            name: name.into(),
            config,
            reconnect_attempt: None,
            max_reconnect_attempts: None,
        })
    }

    /// `{ name, type: 'disabled', config }`.
    pub fn disabled(name: impl Into<String>, config: ScopedMcpServerConfig) -> Self {
        Self::Disabled(DisabledMcpServer {
            name: name.into(),
            config,
        })
    }

    /// A connected record with no connection behind it, declaring no
    /// capabilities.
    #[cfg(test)]
    pub(crate) fn connected_for_test(name: &str, config: ScopedMcpServerConfig) -> Self {
        Self::Connected(ConnectedMcpServer {
            client: super::client::McpClient::detached_for_test(name, &config),
            name: name.to_owned(),
            capabilities: Default::default(),
            server_info: None,
            instructions: None,
            config,
        })
    }

    /// A connected record for a stdio server `name` whose initialize result
    /// carried `instructions`.
    #[cfg(test)]
    pub(crate) fn connected_with_instructions_for_test(name: &str, instructions: &str) -> Self {
        let mut client =
            Self::connected_for_test(name, ScopedMcpServerConfig::stdio_for_test(name));
        if let Self::Connected(connected) = &mut client {
            connected.instructions = Some(instructions.to_owned());
        }
        client
    }
}

#[cfg(test)]
impl ScopedMcpServerConfig {
    /// A user-scoped stdio server running `command`.
    pub(crate) fn stdio_for_test(command: &str) -> Self {
        Self {
            name: None,
            scope: ConfigScope::User,
            transport: Transport::Stdio,
            command: Some(command.to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        }
    }
}

/// Maps to: CC `services/mcp/types.ts:28-35#McpStdioServerConfigSchema`.
pub fn mcp_stdio_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("stdio")).optional()),
            (
                "command",
                zod::string().min_with_message(1, "Command cannot be empty"),
            ),
            (
                "args",
                zod::array(zod::string()).default(serde_json::json!([])),
            ),
            ("env", zod::record(zod::string()).optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:41-41#McpXaaConfigSchema`.
fn mcp_xaa_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(zod::boolean)
}

/// Maps to: CC `services/mcp/types.ts:43-56#McpOAuthConfigSchema`.
fn mcp_oauth_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("clientId", zod::string().optional()),
            ("callbackPort", zod::number().int().positive().optional()),
            (
                "authServerMetadataUrl",
                zod::string()
                    .url()
                    .starts_with_message("https://", "authServerMetadataUrl must use https://")
                    .optional(),
            ),
            ("xaa", mcp_xaa_config_schema().clone().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:58-66#McpSSEServerConfigSchema`.
pub fn mcp_sse_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("sse"))),
            ("url", zod::string()),
            ("headers", zod::record(zod::string()).optional()),
            ("headersHelper", zod::string().optional()),
            ("oauth", mcp_oauth_config_schema().clone().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:69-76#McpSSEIDEServerConfigSchema`.
pub fn mcp_sseide_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("sse-ide"))),
            ("url", zod::string()),
            ("ideName", zod::string()),
            ("ideRunningInWindows", zod::boolean().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:79-87#McpWebSocketIDEServerConfigSchema`.
pub fn mcp_web_socket_ide_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("ws-ide"))),
            ("url", zod::string()),
            ("ideName", zod::string()),
            ("authToken", zod::string().optional()),
            ("ideRunningInWindows", zod::boolean().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:89-97#McpHTTPServerConfigSchema`.
pub fn mcp_http_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("http"))),
            ("url", zod::string()),
            ("headers", zod::record(zod::string()).optional()),
            ("headersHelper", zod::string().optional()),
            ("oauth", mcp_oauth_config_schema().clone().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:99-106#McpWebSocketServerConfigSchema`.
pub fn mcp_web_socket_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("ws"))),
            ("url", zod::string()),
            ("headers", zod::record(zod::string()).optional()),
            ("headersHelper", zod::string().optional()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:108-113#McpSdkServerConfigSchema`.
pub fn mcp_sdk_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("sdk"))),
            ("name", zod::string()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:116-122#McpClaudeAIProxyServerConfigSchema`.
pub fn mcp_claude_ai_proxy_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![
            ("type", zod::literal(serde_json::json!("claudeai-proxy"))),
            ("url", zod::string()),
            ("id", zod::string()),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:124-135#McpServerConfigSchema`.
pub fn mcp_server_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::union(vec![
            mcp_stdio_server_config_schema().clone(),
            mcp_sse_server_config_schema().clone(),
            mcp_sseide_server_config_schema().clone(),
            mcp_web_socket_ide_server_config_schema().clone(),
            mcp_http_server_config_schema().clone(),
            mcp_web_socket_server_config_schema().clone(),
            mcp_sdk_server_config_schema().clone(),
            mcp_claude_ai_proxy_server_config_schema().clone(),
        ])
    })
}

/// Maps to: CC `services/mcp/types.ts:171-175#McpJsonConfigSchema`.
pub fn mcp_json_config_schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        zod::object(vec![(
            "mcpServers",
            zod::record(mcp_server_config_schema().clone()),
        )])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_type_strings_match_official_discriminators() {
        assert_eq!(ConfigScope::Project.as_str(), "project");
        assert_eq!(Transport::Stdio.as_str(), "stdio");
        assert_eq!(Transport::ClaudeAiProxy.as_str(), "claudeai-proxy");
        assert_eq!(McpServerConnectionType::NeedsAuth.as_str(), "needs-auth");
    }

    /// Maps to: CC `services/mcp/types.ts:229`
    /// `ServerResource = Resource & { server: string }`.
    ///
    /// This replaces a test that exercised `from_runtime_snapshot` /
    /// `to_client_snapshot`, the pair that used to convert between this type
    /// and a `server`-less twin. CC never had that twin — the only shape
    /// without `server` is the SDK's own `Resource`, which
    /// `fetchResourcesForClient` (`client.ts:2017-2020`) stamps on the way out
    /// — so both functions and the twin are gone, and there is nothing left to
    /// convert. What remains worth pinning is that `server` is carried, not
    /// derived from the map key it happens to be filed under.
    #[test]
    fn server_resource_carries_its_owning_server_like_official() {
        let resource = ServerResource {
            server: "memory".to_string(),
            uri: "mem://note".to_string(),
            name: "note".to_string(),
            description: Some("desc".to_string()),
            mime_type: Some("text/plain".to_string()),
        };

        assert_eq!(resource.server, "memory");
        assert_eq!(resource.uri, "mem://note");
    }

    #[test]
    fn scoped_mcp_server_config_projects_current_config_shape() {
        let config = crate::utils::config::McpServerConfig {
            command: Some("node".to_string()),
            args: Some(vec!["server.js".to_string()]),
            ..crate::utils::config::McpServerConfig::default()
        };
        let scoped = ScopedMcpServerConfig::from_config(ConfigScope::User, &config);
        assert_eq!(scoped.scope, ConfigScope::User);
        assert_eq!(scoped.transport, Transport::Stdio);
        assert_eq!(scoped.command.as_deref(), Some("node"));
        assert_eq!(scoped.args, vec!["server.js"]);
        assert!(scoped.env.is_empty());
    }
    #[test]
    fn mcp_config_schemas_match_official_bun_oracle() {
        // Actual Bun direct imports of CC utils/plugins/schemas.ts:162-1390 and
        // services/mcp/types.ts:29-136. Check parsed values AND complete ordered
        // Zod issue trees: defaults, transforms, unknown keys, paths and copy.
        // Oracle script: research/proof/plugin-schemas-0914/oracle.ts.
        let cases: serde_json::Value = serde_json::from_str(r###"[{"schema":"McpServerConfigSchema","input":{},"issues":[{"code":"invalid_union","errors":[[{"expected":"string","code":"invalid_type","path":["command"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sse"],"path":["type"],"message":"Invalid input: expected \"sse\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sse-ide"],"path":["type"],"message":"Invalid input: expected \"sse-ide\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws-ide"],"path":["type"],"message":"Invalid input: expected \"ws-ide\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["http"],"path":["type"],"message":"Invalid input: expected \"http\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws"],"path":["type"],"message":"Invalid input: expected \"ws\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sdk"],"path":["type"],"message":"Invalid input: expected \"sdk\""},{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["claudeai-proxy"],"path":["type"],"message":"Invalid input: expected \"claudeai-proxy\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["id"],"message":"Invalid input: expected string, received undefined"}]],"path":[],"message":"Invalid input"}]},{"schema":"McpServerConfigSchema","input":{"command":"echo"},"data":{"command":"echo","args":[]}},{"schema":"McpServerConfigSchema","input":{"command":""},"issues":[{"code":"invalid_union","errors":[[{"origin":"string","code":"too_small","minimum":1,"inclusive":true,"path":["command"],"message":"Command cannot be empty"}],[{"code":"invalid_value","values":["sse"],"path":["type"],"message":"Invalid input: expected \"sse\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sse-ide"],"path":["type"],"message":"Invalid input: expected \"sse-ide\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws-ide"],"path":["type"],"message":"Invalid input: expected \"ws-ide\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["http"],"path":["type"],"message":"Invalid input: expected \"http\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws"],"path":["type"],"message":"Invalid input: expected \"ws\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sdk"],"path":["type"],"message":"Invalid input: expected \"sdk\""},{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["claudeai-proxy"],"path":["type"],"message":"Invalid input: expected \"claudeai-proxy\""},{"expected":"string","code":"invalid_type","path":["url"],"message":"Invalid input: expected string, received undefined"},{"expected":"string","code":"invalid_type","path":["id"],"message":"Invalid input: expected string, received undefined"}]],"path":[],"message":"Invalid input"}]},{"schema":"McpServerConfigSchema","input":{"command":"echo","extra":1},"data":{"command":"echo","args":[]}},{"schema":"McpServerConfigSchema","input":{"type":"sse","url":""},"data":{"type":"sse","url":""}},{"schema":"McpServerConfigSchema","input":{"type":"sse-ide","url":"u","ideName":"i"},"data":{"type":"sse-ide","url":"u","ideName":"i"}},{"schema":"McpServerConfigSchema","input":{"type":"ws-ide","url":"u","ideName":"i","authToken":"a"},"data":{"type":"ws-ide","url":"u","ideName":"i","authToken":"a"}},{"schema":"McpServerConfigSchema","input":{"type":"http","url":"u","oauth":{"authServerMetadataUrl":"http://a"}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":["stdio"],"path":["type"],"message":"Invalid input: expected \"stdio\""},{"expected":"string","code":"invalid_type","path":["command"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sse"],"path":["type"],"message":"Invalid input: expected \"sse\""},{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"https://","path":["oauth","authServerMetadataUrl"],"message":"authServerMetadataUrl must use https://"}],[{"code":"invalid_value","values":["sse-ide"],"path":["type"],"message":"Invalid input: expected \"sse-ide\""},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws-ide"],"path":["type"],"message":"Invalid input: expected \"ws-ide\""},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"https://","path":["oauth","authServerMetadataUrl"],"message":"authServerMetadataUrl must use https://"}],[{"code":"invalid_value","values":["ws"],"path":["type"],"message":"Invalid input: expected \"ws\""}],[{"code":"invalid_value","values":["sdk"],"path":["type"],"message":"Invalid input: expected \"sdk\""},{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["claudeai-proxy"],"path":["type"],"message":"Invalid input: expected \"claudeai-proxy\""},{"expected":"string","code":"invalid_type","path":["id"],"message":"Invalid input: expected string, received undefined"}]],"path":[],"message":"Invalid input"}]},{"schema":"McpServerConfigSchema","input":{"type":"http","url":"u","oauth":{"authServerMetadataUrl":"invalid","callbackPort":0}},"issues":[{"code":"invalid_union","errors":[[{"code":"invalid_value","values":["stdio"],"path":["type"],"message":"Invalid input: expected \"stdio\""},{"expected":"string","code":"invalid_type","path":["command"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["sse"],"path":["type"],"message":"Invalid input: expected \"sse\""},{"origin":"number","code":"too_small","minimum":0,"inclusive":false,"path":["oauth","callbackPort"],"message":"Too small: expected number to be >0"},{"code":"invalid_format","format":"url","path":["oauth","authServerMetadataUrl"],"message":"Invalid URL"},{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"https://","path":["oauth","authServerMetadataUrl"],"message":"authServerMetadataUrl must use https://"}],[{"code":"invalid_value","values":["sse-ide"],"path":["type"],"message":"Invalid input: expected \"sse-ide\""},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["ws-ide"],"path":["type"],"message":"Invalid input: expected \"ws-ide\""},{"expected":"string","code":"invalid_type","path":["ideName"],"message":"Invalid input: expected string, received undefined"}],[{"origin":"number","code":"too_small","minimum":0,"inclusive":false,"path":["oauth","callbackPort"],"message":"Too small: expected number to be >0"},{"code":"invalid_format","format":"url","path":["oauth","authServerMetadataUrl"],"message":"Invalid URL"},{"origin":"string","code":"invalid_format","format":"starts_with","prefix":"https://","path":["oauth","authServerMetadataUrl"],"message":"authServerMetadataUrl must use https://"}],[{"code":"invalid_value","values":["ws"],"path":["type"],"message":"Invalid input: expected \"ws\""}],[{"code":"invalid_value","values":["sdk"],"path":["type"],"message":"Invalid input: expected \"sdk\""},{"expected":"string","code":"invalid_type","path":["name"],"message":"Invalid input: expected string, received undefined"}],[{"code":"invalid_value","values":["claudeai-proxy"],"path":["type"],"message":"Invalid input: expected \"claudeai-proxy\""},{"expected":"string","code":"invalid_type","path":["id"],"message":"Invalid input: expected string, received undefined"}]],"path":[],"message":"Invalid input"}]},{"schema":"McpServerConfigSchema","input":{"type":"ws","url":"u"},"data":{"type":"ws","url":"u"}},{"schema":"McpServerConfigSchema","input":{"type":"sdk","name":"n"},"data":{"type":"sdk","name":"n"}},{"schema":"McpServerConfigSchema","input":{"type":"claudeai-proxy","url":"u","id":"i"},"data":{"type":"claudeai-proxy","url":"u","id":"i"}}]"###).unwrap();
        for case in cases.as_array().unwrap() {
            let schema = match case["schema"].as_str().unwrap() {
                "McpServerConfigSchema" => mcp_server_config_schema(),
                other => panic!("unexpected oracle schema: {other}"),
            };
            match crate::utils::zod::safe_parse(schema, &case["input"]) {
                Ok(data) => {
                    assert!(case.get("data").is_some(), "expected error: {case}");
                    assert_eq!(data, case["data"], "{case}");
                }
                Err(error) => {
                    let issues: Vec<_> = error.issues.iter().map(|issue| issue.to_json()).collect();
                    assert_eq!(serde_json::json!(issues), case["issues"], "{case}");
                }
            }
        }
    }
    #[test]
    fn mcp_sdk_name_and_ide_flag_roundtrip_matches_official_source() {
        // Actual McpServerConfigSchema plus hashMcpConfig/areMcpConfigsEqual:
        // services/mcp/types.ts:69–87,108–113,163–169; utils.ts:157–168;
        // client.ts:1710–1723. Oracle imports the real schema and source bodies.
        let cases: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/oracles/mcp-config-fields-0915/oracle.json"
        )))
        .unwrap();
        for case in cases.as_array().unwrap() {
            let validated = zod::safe_parse(mcp_server_config_schema(), &case["input"]).unwrap();
            assert_eq!(validated, case["parsed"]);
            let raw: crate::utils::config::McpServerConfig =
                serde_json::from_value(validated.clone()).unwrap();
            let serialized = serde_json::to_value(&raw).unwrap();
            // This regression concerns the two previously lost fields; existing
            // optional fields in the broad native DTO are not a new wire format.
            for field in ["name", "ideRunningInWindows"] {
                assert_eq!(
                    serialized.get(field),
                    validated.get(field),
                    "{case}: {field}"
                );
            }
            let restored: crate::utils::config::McpServerConfig =
                serde_json::from_value(serialized).unwrap();
            let scoped = ScopedMcpServerConfig::from_config(ConfigScope::Dynamic, &restored);
            assert_eq!(scoped.name.as_deref(), validated["name"].as_str());
            assert_eq!(
                scoped.ide_running_in_windows,
                validated["ideRunningInWindows"].as_bool()
            );
            if scoped.transport == Transport::Sdk {
                assert!(
                    scoped.id.is_none(),
                    "SDK name is not claude.ai connector id"
                );
            }
            assert_eq!(
                crate::services::mcp::utils::hash_mcp_config(&scoped),
                case["hash"].as_str().unwrap()
            );
            let mut same = scoped.clone();
            same.scope = ConfigScope::User;
            assert_eq!(
                crate::services::mcp::client::are_mcp_configs_equal(&scoped, &same),
                case["sameAfterScope"].as_bool().unwrap()
            );
            let mut changed = scoped.clone();
            if changed.transport == Transport::Sdk {
                changed.name = Some("changed".into());
            } else {
                changed.ide_running_in_windows = Some(changed.ide_running_in_windows != Some(true));
            }
            assert_eq!(
                crate::services::mcp::utils::hash_mcp_config(&changed),
                case["changedHash"].as_str().unwrap()
            );
            assert_eq!(
                crate::services::mcp::client::are_mcp_configs_equal(&scoped, &changed),
                case["sameAfterChange"].as_bool().unwrap()
            );
        }
    }
}
