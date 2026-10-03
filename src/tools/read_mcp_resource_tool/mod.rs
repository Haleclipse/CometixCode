//! Incremental port of the official MCP resource reading tool.
//!
//! Schema/prompt metadata maps to:
//! - CC `tools/ReadMcpResourceTool/ReadMcpResourceTool.ts`
//!
//! UI helpers remain display-only. The behavioral `ToolCall` implementation
//! lives in this tool module and dispatches live resource reads through
//! `services/mcp/client.rs`, matching the official tool/service boundary.

pub mod prompt;
pub mod ui;

/// Maps to: CC `ReadMcpResourceTool.ts:22-27` `inputSchema`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::object(vec![
            ("server", zod::string().describe("The MCP server name")),
            ("uri", zod::string().describe("The resource URI to read")),
        ])
    })
}

/// Maps to: CC `ReadMcpResourceTool` metadata.
pub fn read_mcp_resource_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::READ_MCP_RESOURCE_TOOL_NAME.to_string(),
        description: prompt::READ_MCP_RESOURCE_PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `ReadMcpResourceTool.ts:32-42` inline `contents` element (anonymous
/// `z.object`, so the name is ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob_saved_to: Option<String>,
}

/// Maps to: CC `tools/ReadMcpResourceTool/ReadMcpResourceTool.ts:47`
/// `export type Output = z.infer<OutputSchema>` (schema at :30-44); the
/// render path recovers it via `outputSchema.safeParse`
/// ([`ui::parse_output`] is the Rust stand-in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub contents: Vec<ResourceContent>,
}

fn read_mcp_resource_blob_content_output(
    server_name: &str,
    uri: String,
    mime_type: Option<String>,
    blob: &str,
    index: usize,
) -> ResourceContent {
    // Maps to: CC `ReadMcpResourceTool.call(...)` binary `blob` interception:
    // decode base64, persist raw bytes through `persistBinaryContent(...)`, and
    // replace the context payload with `blobSavedTo` plus the saved-file text.
    use base64::Engine as _;
    let bytes = match base64::engine::general_purpose::STANDARD.decode(blob.as_bytes()) {
        Ok(bytes) => bytes,
        Err(error) => {
            return ResourceContent {
                uri,
                mime_type,
                text: Some(format!(
                    "Binary content could not be saved to disk: {error}"
                )),
                blob_saved_to: None,
            };
        }
    };
    // CC persist id shape: `mcp-resource-${Date.now()}-${i}-${random 6 chars}`
    // (Math.random().toString(36).slice(2, 8)); millis + a 6-char base36-ish
    // suffix is the same observable filename shape.
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect();
    let persist_id = format!("mcp-resource-{millis}-{index}-{suffix}");
    match crate::utils::mcp_output_storage::persist_binary_content(
        &bytes,
        mime_type.as_deref(),
        &persist_id,
    ) {
        crate::utils::mcp_output_storage::PersistBinaryResult::Saved(saved) => {
            let filepath = saved.filepath.to_string_lossy().to_string();
            ResourceContent {
                uri: uri.clone(),
                mime_type: mime_type.clone(),
                blob_saved_to: Some(filepath.clone()),
                text: Some(
                    crate::utils::mcp_output_storage::get_binary_blob_saved_message(
                        &filepath,
                        mime_type.as_deref(),
                        saved.size,
                        &format!("[Resource from {server_name} at {uri}] "),
                    ),
                ),
            }
        }
        crate::utils::mcp_output_storage::PersistBinaryResult::Error { error } => ResourceContent {
            uri,
            mime_type,
            text: Some(format!(
                "Binary content could not be saved to disk: {error}"
            )),
            blob_saved_to: None,
        },
    }
}

fn read_mcp_resource_result_from_value(server_name: &str, value: serde_json::Value) -> Output {
    let contents = value
        .get("contents")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, content)| {
            let uri = content
                .get("uri")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let mime_type = content
                .get("mimeType")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            if let Some(text) = content.get("text").and_then(|value| value.as_str()) {
                return ResourceContent {
                    uri,
                    mime_type,
                    text: Some(text.to_string()),
                    blob_saved_to: None,
                };
            }
            if let Some(blob) = content.get("blob").and_then(|value| value.as_str()) {
                return read_mcp_resource_blob_content_output(
                    server_name,
                    uri,
                    mime_type,
                    blob,
                    index,
                );
            }
            ResourceContent {
                uri,
                mime_type,
                text: None,
                blob_saved_to: None,
            }
        })
        .collect();
    Output { contents }
}

/// Read an MCP resource via the live rmcp client.
/// Maps to: CC `tools/ReadMcpResourceTool/ReadMcpResourceTool.ts` `call` (:75).
pub(crate) async fn read_mcp_resource_output(
    input: &serde_json::Value,
    state: &crate::state::app_state_store::McpState,
) -> Result<Output, String> {
    // CC destructures both required fields and uses them verbatim — no
    // trimming, no empty-string collapse (an empty server simply fails the
    // find with the "not found" error).
    let server = input
        .get("server")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let uri = input
        .get("uri")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    let available_servers = state
        .clients
        .iter()
        .map(|candidate| candidate.client.name.as_str())
        .collect::<Vec<_>>();
    let Some(client) = state
        .clients
        .iter()
        .find(|candidate| candidate.client.name == server)
    else {
        return Err(format!(
            "Server \"{server}\" not found. Available servers: {}",
            available_servers.join(", ")
        ));
    };
    if client.client.status != crate::services::mcp::types::McpServerConnectionType::Connected {
        return Err(format!("Server \"{server}\" is not connected"));
    }
    if !client.supports_resources {
        return Err(format!("Server \"{server}\" does not support resources"));
    }

    crate::services::mcp::client::read_mcp_resource(server, uri)
        .await
        .map(|value| read_mcp_resource_result_from_value(server, value))
        .map_err(|error| error.to_string())
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape —
/// optional fields absent, never null.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    serde_json::json!({
        "contents": output
            .contents
            .iter()
            .map(|content| {
                let mut object = serde_json::Map::new();
                object.insert("uri".to_string(), serde_json::json!(&content.uri));
                if let Some(mime_type) = &content.mime_type {
                    object.insert("mimeType".to_string(), serde_json::json!(mime_type));
                }
                // Official key order: the persisted blob branch emits
                // `blobSavedTo` before `text` (`ReadMcpResourceTool.ts:127-137`).
                if let Some(blob_saved_to) = &content.blob_saved_to {
                    object.insert("blobSavedTo".to_string(), serde_json::json!(blob_saved_to));
                }
                if let Some(text) = &content.text {
                    object.insert("text".to_string(), serde_json::json!(text));
                }
                serde_json::Value::Object(object)
            })
            .collect::<Vec<_>>()
    })
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

/// Behavioral half of CC `ReadMcpResourceTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ReadMcpResourceTool;

impl crate::tool::ToolCall for ReadMcpResourceTool {
    fn name(&self) -> &'static str {
        "ReadMcpResourceTool"
    }

    /// Maps to: CC `ReadMcpResourceTool.ts:66-68` `async prompt() { return
    /// PROMPT }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::READ_MCP_RESOURCE_PROMPT.to_string()
    }

    /// Maps to: CC `ReadMcpResourceTool.isConcurrencySafe()` (:50-52).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ReadMcpResourceTool.isReadOnly()` (:53-55).
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ReadMcpResourceTool.shouldDefer` (:59).
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `ReadMcpResourceTool.searchHint` (:61).
    fn search_hint(&self) -> Option<&'static str> {
        Some("read a specific MCP resource by URI")
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `ReadMcpResourceTool/UI.tsx:20-22` `userFacingName()`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "readMcpResource".to_string()
    }

    /// Maps to: CC `ReadMcpResourceTool.toAutoClassifierInput` (:56-58).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        format!(
            "{} {}",
            args.get("server")
                .and_then(|value| value.as_str())
                .unwrap_or_default(),
            args.get("uri")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        )
    }

    /// Maps to: CC `ReadMcpResourceTool.isResultTruncated` (:148-150) —
    /// `isOutputLineTruncated(jsonStringify(output))`.
    fn is_result_truncated(&self, data: &crate::tool::ToolOutput) -> bool {
        match data {
            crate::tool::ToolOutput::ReadMcpResource(output) => {
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
            match read_mcp_resource_output(args, &context.mcp_state).await {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::ReadMcpResource(output),
                    new_messages: Vec::new(),
                },
                Err(error) => mcp_resource_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/ReadMcpResourceTool/ReadMcpResourceTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:151-157).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        use crate::types::message::ToolResultStatus;
        match data {
            crate::tool::ToolOutput::ReadMcpResource(output) => (
                output_to_value(output).to_string(),
                ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (String::new(), ToolResultStatus::Error),
        }
    }

    /// Maps to: CC recording ReadMcpResourceTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::ReadMcpResource(output) => Some(output_to_value(output)),
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
    fn read_mcp_resource_binary_blob_persists_like_official_tool() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let root = std::env::temp_dir().join(format!(
            "cometix-read-mcp-resource-{}",
            uuid::Uuid::new_v4()
        ));
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(&root);
        crate::bootstrap::state::set_original_cwd("/tmp/cometix-project");
        crate::bootstrap::state::set_session_id("session-mcp-resource-binary-test");

        let output = super::read_mcp_resource_result_from_value(
            "docs",
            serde_json::json!({
                "contents": [{
                    "uri": "file://manual.pdf",
                    "mimeType": "application/pdf",
                    "blob": "JVBERi1yYXc="
                }]
            }),
        );

        assert_eq!(output.contents.len(), 1);
        let content = &output.contents[0];
        let saved_path = content.blob_saved_to.as_ref().expect("saved path");
        assert!(saved_path.ends_with(".pdf"), "saved_path={saved_path}");
        assert_eq!(std::fs::read(saved_path).unwrap(), b"%PDF-raw");
        assert!(
            content.text.as_ref().unwrap().contains(
                "[Resource from docs at file://manual.pdf] Binary content (application/pdf"
            )
        );
        assert!(content.text.as_ref().unwrap().contains("saved to"));

        crate::bootstrap::state::set_original_cwd(previous_cwd);
        crate::bootstrap::state::set_session_id(previous_session_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn read_mcp_resource_reports_not_connected_like_official() {
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

        let error = super::read_mcp_resource_output(
            &serde_json::json!({"server":"offline", "uri":"mem://note"}),
            &state,
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Server \"offline\" is not connected");
    }
}
