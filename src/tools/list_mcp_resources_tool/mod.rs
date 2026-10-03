//! Incremental port of the official MCP resource listing tool.
//!
//! Schema/prompt metadata maps to:
//! - CC `tools/ListMcpResourcesTool/ListMcpResourcesTool.ts`
//!
//! UI helpers remain display-only. The behavioral `ToolCall` implementation
//! lives in this tool module and dispatches live resource listing through
//! `services/mcp/client.rs`, matching the official tool/service boundary.

pub mod prompt;
pub mod ui;

/// Maps to: CC `ListMcpResourcesTool.ts:15-22` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![(
            "server",
            zod::string()
                .optional()
                .describe("Optional server name to filter resources by"),
        )])
    })
}

/// Maps to: CC `ListMcpResourcesTool` metadata.
pub fn list_mcp_resources_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::LIST_MCP_RESOURCES_TOOL_NAME.to_string(),
        description: prompt::LIST_MCP_RESOURCES_PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `ListMcpResourcesTool.ts:25-35` inline array element (anonymous
/// `z.object`, so the name is ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub mime_type: Option<String>,
    pub description: Option<String>,
    pub server: String,
}

/// Maps to: CC `tools/ListMcpResourcesTool/ListMcpResourcesTool.ts:38`
/// `export type Output = z.infer<OutputSchema>` — a top-level ARRAY of
/// resources (schema at :25-35); the render path recovers it via
/// `outputSchema.safeParse` ([`ui::parse_output`] is the Rust stand-in).
pub type Output = Vec<McpResource>;

const NO_MCP_RESOURCES_COPY: &str =
    "No resources found. MCP servers may still provide tools even if they have no resources.";

/// List MCP resources from live connected clients.
/// Maps to: CC `tools/ListMcpResourcesTool/ListMcpResourcesTool.ts` `call`
/// using `ensureConnectedClient(client)` and `fetchResourcesForClient(fresh)`.
pub(crate) async fn list_mcp_resources_output(
    input: &serde_json::Value,
    state: &crate::state::app_state_store::McpState,
) -> Result<Output, String> {
    // CC `targetServer ?` is a JS truthiness test — the empty string falls
    // through to "all servers", but any other value (whitespace included)
    // filters verbatim.
    let target_server = input
        .get("server")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());

    let available_servers = state
        .clients
        .iter()
        .map(|server| server.client.name.as_str())
        .collect::<Vec<_>>();

    if let Some(server) = target_server {
        if !available_servers.iter().any(|name| *name == server) {
            return Err(format!(
                "Server \"{server}\" not found. Available servers: {}",
                available_servers.join(", ")
            ));
        }
    }

    let mut output = Vec::new();
    for server in state
        .clients
        .iter()
        .filter(|server| {
            server.client.status == crate::services::mcp::types::McpServerConnectionType::Connected
        })
        .filter(|server| target_server.is_none_or(|target| target == server.client.name))
    {
        // `fetch_mcp_resources_for_client` is LRU-cached (by server name) and
        // already warm from startup prefetch. The cache is invalidated on close
        // and on `resources/list_changed`, so results are never stale.
        match crate::services::mcp::client::fetch_mcp_resources_for_client(&server.client.name)
            .await
        {
            Ok(resources) => output.extend(resources.iter().map(|resource| McpResource {
                uri: resource.uri.clone(),
                name: resource.name.clone(),
                mime_type: resource.mime_type.clone(),
                description: resource.description.clone(),
                server: server.client.name.clone(),
            })),
            Err(error) => {
                tracing::warn!(server = %server.client.name, error = %error, "failed to refresh MCP resources for ListMcpResourcesTool")
            }
        }
    }
    Ok(output)
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape —
/// optional fields absent, never null.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    serde_json::Value::Array(
        output
            .iter()
            .map(|resource| {
                let mut object = serde_json::Map::new();
                object.insert("uri".to_string(), serde_json::json!(&resource.uri));
                object.insert("name".to_string(), serde_json::json!(&resource.name));
                if let Some(mime_type) = &resource.mime_type {
                    object.insert("mimeType".to_string(), serde_json::json!(mime_type));
                }
                if let Some(description) = &resource.description {
                    object.insert("description".to_string(), serde_json::json!(description));
                }
                object.insert("server".to_string(), serde_json::json!(&resource.server));
                serde_json::Value::Object(object)
            })
            .collect(),
    )
}

fn mcp_resource_error_result(error: String) -> crate::tool::ToolResult {
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::Composed {
            content: error,
            status: crate::types::message::ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

/// Behavioral half of CC `ListMcpResourcesTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ListMcpResourcesTool;

impl crate::tool::ToolCall for ListMcpResourcesTool {
    fn name(&self) -> &'static str {
        "ListMcpResourcesTool"
    }

    /// Maps to: CC `ListMcpResourcesTool.ts:57-59` `async prompt() { return
    /// PROMPT }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::LIST_MCP_RESOURCES_PROMPT.to_string()
    }

    /// Maps to: CC `ListMcpResourcesTool.isConcurrencySafe()` (:41-43).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ListMcpResourcesTool.isReadOnly()` (:44-46).
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ListMcpResourcesTool.shouldDefer` (:50).
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `ListMcpResourcesTool.searchHint` (:52).
    fn search_hint(&self) -> Option<&'static str> {
        Some("list resources from connected MCP servers")
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `ListMcpResourcesTool.userFacingName()` (:103).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "listMcpResources".to_string()
    }

    /// Maps to: CC `ListMcpResourcesTool.toAutoClassifierInput` (:47-49).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        args.get("server")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// Maps to: CC `ListMcpResourcesTool.isResultTruncated` (:105-107) —
    /// `isOutputLineTruncated(jsonStringify(output))`.
    fn is_result_truncated(&self, data: &crate::tool::ToolOutput) -> bool {
        match data {
            crate::tool::ToolOutput::ListMcpResources(output) => {
                crate::utils::terminal::is_output_line_truncated(
                    &output_to_value(output).to_string(),
                )
            }
            _ => false,
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match list_mcp_resources_output(args, &context.mcp_state).await {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::ListMcpResources(output),
                    new_messages: Vec::new(),
                },
                Err(error) => mcp_resource_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/ListMcpResourcesTool/ListMcpResourcesTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:108-122).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::ListMcpResources(output) => {
                if output.is_empty() {
                    (NO_MCP_RESOURCES_COPY.to_string(), ToolResultStatus::Success)
                } else {
                    (
                        output_to_value(output).to_string(),
                        ToolResultStatus::Success,
                    )
                }
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Maps to: CC recording ListMcpResourcesTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::ListMcpResources(output) => Some(output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn mcp_resource_tool_schemas_match_official_names_and_inputs() {
        let list = super::list_mcp_resources_tool_schema();
        assert_eq!(list.name, "ListMcpResourcesTool");
        // CC declares `userFacingName: () => 'listMcpResources'`, not an alias.
        assert!(list.aliases.is_empty());
        assert_eq!(
            crate::tool::ToolCall::user_facing_name(&super::ListMcpResourcesTool, None),
            "listMcpResources"
        );
        // `server` is optional, so zod emits no `required` at all
        // (cc_input_schemas.json:555-568).
        assert_eq!(list.input_schema.get("required"), None);
        assert!(list.description.contains("List available resources"));

        let read = crate::tools::read_mcp_resource_tool::read_mcp_resource_tool_schema();
        assert_eq!(read.name, "ReadMcpResourceTool");
        assert!(read.aliases.is_empty());
        assert_eq!(
            crate::tool::ToolCall::user_facing_name(
                &crate::tools::read_mcp_resource_tool::ReadMcpResourceTool,
                None
            ),
            "readMcpResource"
        );
        assert_eq!(
            read.input_schema.get("required"),
            Some(&serde_json::json!(["server", "uri"]))
        );
        assert!(read.description.contains("identified by server name"));
    }

    #[test]
    fn mcp_resource_tool_mappers_follow_official_model_content_shapes() {
        let list_output = vec![super::McpResource {
            uri: "mem://note".to_string(),
            name: "note".to_string(),
            mime_type: Some("text/plain".to_string()),
            description: Some("demo".to_string()),
            server: "memory".to_string(),
        }];
        let data = crate::tool::ToolOutput::ListMcpResources(list_output);
        let (content, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &super::ListMcpResourcesTool,
            &data,
            "toolu_mcp_list",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json[0]["uri"], "mem://note");
        assert_eq!(json[0]["mimeType"], "text/plain");

        let empty = crate::tool::ToolOutput::ListMcpResources(Vec::new());
        let (content, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &super::ListMcpResourcesTool,
            &empty,
            "toolu_mcp_list",
        );
        assert_eq!(
            content,
            "No resources found. MCP servers may still provide tools even if they have no resources."
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);

        let read_output = crate::tools::read_mcp_resource_tool::Output {
            contents: vec![crate::tools::read_mcp_resource_tool::ResourceContent {
                uri: "mem://note".to_string(),
                mime_type: Some("text/plain".to_string()),
                text: Some("hello".to_string()),
                blob_saved_to: None,
            }],
        };
        let data = crate::tool::ToolOutput::ReadMcpResource(read_output);
        let (content, status) = crate::tool::ToolCall::map_tool_result_to_tool_result_block_param(
            &crate::tools::read_mcp_resource_tool::ReadMcpResourceTool,
            &data,
            "toolu_mcp_read",
        );
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["contents"][0]["text"], "hello");
    }

    #[tokio::test]
    async fn list_mcp_resources_validates_target_and_skips_non_connected_clients_like_official() {
        let state = crate::state::app_state_store::McpState {
            clients: vec![crate::services::mcp::types::McpServerSnapshot {
                connection_id: None,
                client: crate::services::mcp::types::McpClientSnapshot {
                    name: "offline".to_string(),
                    status: crate::services::mcp::types::McpServerConnectionType::Failed,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: Some("boom".to_string()),
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: Vec::new(),
                resources: Vec::new(),
            }],
            ..crate::state::app_state_store::McpState::default()
        };

        let offline_output =
            super::list_mcp_resources_output(&serde_json::json!({"server":"offline"}), &state)
                .await
                .expect("official list returns an empty result for non-connected selected clients");
        assert!(offline_output.is_empty());
        let missing_error =
            super::list_mcp_resources_output(&serde_json::json!({"server":"missing"}), &state)
                .await
                .unwrap_err();
        assert!(missing_error.contains("Available servers: offline"));
    }

    /// Official routes the tool through the LRU-memoized
    /// `fetchResourcesForClient` (`ListMcpResourcesTool.ts:89`), whose cache is
    /// invalidated only by `onclose` and `resources/list_changed`. So the first
    /// call populates from the live peer and repeat calls are served from cache
    /// without another `resources/list` round trip.
    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn list_mcp_resources_tool_serves_lru_cached_resources_like_official() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-list-mcp-resources-live-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
let listCalls = 0
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
rl.on('line', line => {
  let message
  try { message = JSON.parse(line) } catch { return }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: { resources: {} },
        serverInfo: { name: 'list-resource-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'resources/list') {
    listCalls += 1
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        resources: [{
          uri: `file:///tmp/call-${listCalls}.txt`,
          name: `call-${listCalls}`,
          description: 'Live resource',
          mimeType: 'text/plain'
        }]
      }
    })
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#,
        )
        .expect("write MCP fixture");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                const SERVER: &str = "list-resources-live-stdio-fixture";
                let config = crate::services::mcp::types::ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: crate::services::mcp::types::Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
                    env: std::collections::BTreeMap::new(),
                    url: None,
                    headers: std::collections::BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery =
                    crate::services::mcp::client::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    crate::services::mcp::types::McpServerConnectionType::Connected
                );

                let state = crate::state::app_state_store::McpState {
                    clients: vec![crate::services::mcp::types::McpServerSnapshot {
                        connection_id: None,
                        client: discovery.server.client.clone(),
                        config: Some(config.clone()),
                        supports_resources: true,
                        tools: Vec::new(),
                        prompts: Vec::new(),
                        resources: vec![crate::services::mcp::types::ServerResource {
                            server: SERVER.to_string(),
                            uri: "file:///tmp/stale.txt".to_string(),
                            name: "stale".to_string(),
                            description: Some("Stale cached resource".to_string()),
                            mime_type: Some("text/plain".to_string()),
                        }],
                    }],
                    ..crate::state::app_state_store::McpState::default()
                };

                // `connect_to_server` already warmed the memoized fetch, so the
                // tool observes the startup prefetch rather than the stale
                // AppState snapshot or a second `resources/list`.
                let output = super::list_mcp_resources_output(
                    &serde_json::json!({ "server": SERVER }),
                    &state,
                )
                .await
                .expect("list should read the memoized live resources");
                assert_eq!(output.len(), 1);
                assert_eq!(output[0].server, SERVER);
                assert_eq!(output[0].uri, "file:///tmp/call-1.txt");

                let repeat = super::list_mcp_resources_output(
                    &serde_json::json!({ "server": SERVER }),
                    &state,
                )
                .await
                .expect("repeat list should hit the LRU cache");
                assert_eq!(repeat, output);

                // `resources/list_changed` is one of the two official
                // invalidation points; after it the next fetch is fresh.
                let refreshed =
                    crate::services::mcp::client::refresh_mcp_resources_for_client(SERVER)
                        .await
                        .expect("refresh should bypass and repopulate the cache");
                assert_eq!(refreshed.len(), 1);
                assert_eq!(refreshed[0].uri, "file:///tmp/call-2.txt");

                crate::services::mcp::client::clear_server_cache(SERVER, None).await;
                let _ = crate::services::mcp::client::drain_mcp_connection_callback_observations()
                    .await;
            });
        let _ = std::fs::remove_file(script_path);
    }
}
