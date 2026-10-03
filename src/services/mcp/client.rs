//! MCP client layer.
//! Maps to: CC `services/mcp/client.ts`.
//!
//! Claude Code uses `@modelcontextprotocol/sdk`'s `Client`,
//! `StdioClientTransport`, and `StreamableHTTPClientTransport`. Cometix maps
//! those responsibilities to the official Rust SDK for MCP
//! (`modelcontextprotocol/rust-sdk`, crate `rmcp`). This module owns transport
//! startup, client connection, tool/prompt/resource discovery, reconnect cache
//! invalidation, and MCP tool calls. Session callbacks
//! (`onclose` / notification handlers) are owned by
//! [`crate::services::mcp::use_manage_mcp_connections`] (CC `onConnectionAttempt`). UI
//! components and slash commands must not instantiate transports directly.

use super::elicitation_handler::{
    ElicitationAction, ElicitationRequestEvent, ElicitationRequestParams, ElicitationResult,
    reduce_elicitation_hook_results, reduce_elicitation_result_hook_results,
    url_elicitation_required_result_message,
};
use super::types::{
    McpClientSnapshot, McpPromptSnapshot, McpServerConnectionType, McpServerSnapshot,
    McpToolSnapshot, ScopedMcpServerConfig, ServerResource, Transport,
};
use crate::state::app_state_store::McpState;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;
// CC's `String(...)` / template-literal conversion on MCP payload fields
// (`client.ts:2505` `String(resultContent.data)`, `:2670`
// `String(result.toolResult)`, and the `${resource.uri}` / `${resource.text}` /
// `${resourceLink.name}` templates inside `transformResultContent`). The
// conversion belongs to the zod carrier's `z.coerce.string()` port, which is the
// only place `Number::toString` (`1`, not `1.0`), `[object Object]`, and the
// array comma-join are implemented.
use crate::utils::zod::js_string;
use serde_json::{Map, Value};

/// Server-authored `InitializeResult.instructions` for live MCP connections.
/// Maps to the `ConnectedMCPServer.instructions` field consumed by
/// `utils/mcpInstructionsDelta.ts`; the running-client registry is the Rust
/// equivalent of CC's connection objects.
static MCP_SERVER_INSTRUCTIONS: std::sync::LazyLock<
    std::sync::RwLock<std::collections::BTreeMap<String, String>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::BTreeMap::new()));

fn set_mcp_server_instructions(name: &str, instructions: Option<String>) {
    let mut state = MCP_SERVER_INSTRUCTIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(instructions) = instructions.filter(|value| !value.is_empty()) {
        state.insert(name.to_string(), instructions);
    } else {
        state.remove(name);
    }
}

fn clear_mcp_server_instructions(name: &str) {
    MCP_SERVER_INSTRUCTIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(name);
}

/// Snapshot server-authored instructions for currently connected AppState
/// clients. Disconnected stale registry entries are never exposed.
pub(crate) fn connected_mcp_server_instructions(
    state: &McpState,
) -> std::collections::BTreeMap<String, String> {
    let connected = state
        .clients
        .iter()
        .filter(|server| server.client.status == McpServerConnectionType::Connected)
        .map(|server| server.client.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    MCP_SERVER_INSTRUCTIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|(name, _)| connected.contains(name.as_str()))
        .map(|(name, instructions)| (name.clone(), instructions.clone()))
        .collect()
}

#[cfg(test)]
pub(crate) static TEST_MCP_INSTRUCTIONS_LOCK: std::sync::LazyLock<TestStateLock> =
    std::sync::LazyLock::new(TestStateLock::new);

#[cfg(test)]
pub(crate) fn set_mcp_server_instructions_for_test(name: &str, instructions: Option<&str>) {
    set_mcp_server_instructions(name, instructions.map(str::to_string));
}

/// Maps to: CC MCP client lifecycle callbacks that feed `updateServer(...)`.
/// Prefer [`crate::services::mcp::use_manage_mcp_connections::OnConnectionAttemptHandlers`]
/// registered by App (CC `onConnectionAttempt` closures). Observation drain is for tests without App.
pub use crate::services::mcp::use_manage_mcp_connections::drain_mcp_connection_callback_observations;

/// Maps to: CC `interactiveHandler.ts` outbound channel permission relay
/// `client.client.notification({ method: CHANNEL_PERMISSION_REQUEST_METHOD, params })`.
pub async fn send_channel_permission_request_to_relays(
    params: &crate::services::mcp::channel_permissions::ChannelPermissionRequestParams,
) -> crate::services::mcp::channel_permissions::ChannelPermissionRelaySendReport {
    runtime::send_channel_permission_request_to_relays(params).await
}

/// Maps to: CC `ElicitationRequestEvent.respond(...)` callback stored in
/// `services/mcp/elicitationHandler.ts`.
pub async fn respond_to_mcp_elicitation(
    server_name: &str,
    request_id: &str,
    result: ElicitationResult,
) -> bool {
    runtime::respond_to_mcp_elicitation(server_name, request_id, result).await
}

/// Maps to: CC `DEFAULT_MCP_TOOL_TIMEOUT_MS`.
pub const DEFAULT_MCP_TOOL_TIMEOUT_MS: u64 = 100_000_000;

/// Maps to: CC `getMcpToolTimeoutMs()`.
pub fn get_mcp_tool_timeout_ms_from_env(get_env: impl Fn(&str) -> Option<String>) -> u64 {
    get_env("MCP_TOOL_TIMEOUT")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MCP_TOOL_TIMEOUT_MS)
}

/// Maps to: CC `services/mcp/client.ts#areMcpConfigsEqual`.
pub fn are_mcp_configs_equal(a: &ScopedMcpServerConfig, b: &ScopedMcpServerConfig) -> bool {
    if a.transport != b.transport {
        return false;
    }

    // CC excludes `scope` because it is provenance metadata, not connection
    // config. The Rust projection compares the remaining connection fields
    // explicitly instead of relying on struct-level PartialEq (which includes
    // scope).
    a.name == b.name
        && a.ide_running_in_windows == b.ide_running_in_windows
        && a.command == b.command
        && a.args == b.args
        && a.env == b.env
        && a.url == b.url
        && a.headers == b.headers
        && a.headers_helper == b.headers_helper
        && a.oauth == b.oauth
        && a.ide_name == b.ide_name
        && a.auth_token == b.auth_token
        && a.id == b.id
        && a.plugin_source == b.plugin_source
}

/// Maps to: CC `mcpToolInputToAutoClassifierInput(input, toolName)`.
pub fn mcp_tool_input_to_auto_classifier_input(
    input: &Map<String, Value>,
    tool_name: &str,
) -> String {
    if input.is_empty() {
        return tool_name.to_string();
    }
    input
        .iter()
        .map(|(key, value)| format!("{key}={}", value_to_classifier_string(value)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn value_to_classifier_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

/// Maps to: CC `callMCPTool(...)` `_meta` payload carrying
/// `claudecode/toolUseId` from `extractToolUseId(parentMessage)`.
pub fn mcp_tool_use_id_meta(tool_use_id: &str) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(
        "claudecode/toolUseId".to_string(),
        Value::String(tool_use_id.to_string()),
    );
    meta
}

/// Maps to: CC `services/mcp/client.ts` `inferCompactSchema(value, depth = 2)`.
pub fn infer_compact_schema(value: &Value, depth: i32) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Array(values) => values
            .first()
            .map(|first| format!("[{}]", infer_compact_schema(first, depth - 1)))
            .unwrap_or_else(|| "[]".to_string()),
        Value::Object(object) => {
            if depth <= 0 {
                return "{...}".to_string();
            }
            let props = object
                .iter()
                .take(10)
                .map(|(key, value)| format!("{key}: {}", infer_compact_schema(value, depth - 1)))
                .collect::<Vec<_>>()
                .join(", ");
            let suffix = if object.len() > 10 { ", ..." } else { "" };
            format!("{{{props}{suffix}}}")
        }
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(_) => "string".to_string(),
    }
}

/// Maps to: CC `services/mcp/client.ts` `TransformedMCPResult`.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformedMcpResult {
    pub content: Value,
    pub result_type: crate::utils::mcp_output_storage::McpResultType,
    pub schema: Option<String>,
}

fn json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn mcp_image_mime_type(mime_type: Option<&str>) -> String {
    let ext = mime_type
        .and_then(|mime_type| mime_type.split('/').nth(1))
        .filter(|ext| !ext.is_empty())
        .unwrap_or("png");
    format!("image/{ext}")
}

fn is_mcp_image_mime_type(mime_type: Option<&str>) -> bool {
    matches!(
        mime_type,
        Some("image/jpeg" | "image/png" | "image/gif" | "image/webp")
    )
}

fn mcp_text_content_block(text: String) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}

fn mcp_image_content_block(data: String, mime_type: Option<&str>) -> Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "data": data,
            "media_type": mcp_image_mime_type(mime_type),
            "type": "base64",
        }
    })
}

fn mcp_resized_image_content_block(data: String, mime_type: Option<&str>) -> anyhow::Result<Value> {
    let resized =
        crate::utils::image_resizer::maybe_resize_and_downsample_image_base64(&data, mime_type)?;
    Ok(mcp_image_content_block(
        crate::utils::image_resizer::resize_result_base64(&resized),
        Some(&resized.media_type),
    ))
}

fn decode_mcp_base64(data: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|error| anyhow::anyhow!(error))
}

/// Maps to: CC `services/mcp/client.ts` `persistBlobToTextBlock(...)`.
fn persist_blob_to_text_block(
    bytes: anyhow::Result<Vec<u8>>,
    mime_type: Option<&str>,
    server_name: &str,
    source_description: &str,
) -> Vec<Value> {
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            return vec![mcp_text_content_block(format!(
                "{source_description}Binary content ({}, 0 bytes) could not be saved to disk: {error}",
                mime_type.unwrap_or("unknown type")
            ))];
        }
    };
    let persist_id = format!(
        "mcp-{}-blob-{}",
        crate::services::mcp::normalization::normalize_name_for_mcp(server_name),
        uuid::Uuid::new_v4()
    );
    match crate::utils::mcp_output_storage::persist_binary_content(&bytes, mime_type, &persist_id) {
        crate::utils::mcp_output_storage::PersistBinaryResult::Saved(result) => {
            vec![mcp_text_content_block(
                crate::utils::mcp_output_storage::get_binary_blob_saved_message(
                    &result.filepath.to_string_lossy(),
                    mime_type,
                    result.size,
                    source_description,
                ),
            )]
        }
        crate::utils::mcp_output_storage::PersistBinaryResult::Error { error } => {
            vec![mcp_text_content_block(format!(
                "{source_description}Binary content ({}, {} bytes) could not be saved to disk: {error}",
                mime_type.unwrap_or("unknown type"),
                bytes.len()
            ))]
        }
    }
}

/// Maps to: CC `services/mcp/client.ts` `transformResultContent(...)`.
pub fn transform_result_content(
    result_content: &Value,
    server_name: &str,
) -> anyhow::Result<Vec<Value>> {
    let Some(content_type) = result_content.get("type").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    match content_type {
        "text" => Ok(vec![mcp_text_content_block(
            result_content
                .get("text")
                .map(js_string)
                .unwrap_or_default(),
        )]),
        "audio" => {
            let data = result_content
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mime_type = result_content.get("mimeType").and_then(Value::as_str);
            Ok(persist_blob_to_text_block(
                decode_mcp_base64(data),
                mime_type,
                server_name,
                &format!("[Audio from {server_name}] "),
            ))
        }
        "image" => {
            let data = result_content
                .get("data")
                .map(js_string)
                .unwrap_or_default();
            let mime_type = result_content.get("mimeType").and_then(Value::as_str);
            // Maps to: CC `transformResultContent(...)` image case calling
            // `maybeResizeAndDownsampleImageBuffer(...)` before emitting the
            // API image block.
            Ok(vec![mcp_resized_image_content_block(data, mime_type)?])
        }
        "resource" => {
            let Some(resource) = result_content.get("resource").and_then(Value::as_object) else {
                return Ok(Vec::new());
            };
            let uri = resource.get("uri").map(js_string).unwrap_or_default();
            let prefix = format!("[Resource from {server_name} at {uri}] ");
            if let Some(text) = resource.get("text") {
                return Ok(vec![mcp_text_content_block(format!(
                    "{prefix}{}",
                    js_string(text)
                ))]);
            }
            if let Some(blob) = resource.get("blob") {
                let blob = js_string(blob);
                let mime_type = resource.get("mimeType").and_then(Value::as_str);
                if is_mcp_image_mime_type(mime_type) {
                    return Ok(vec![
                        mcp_text_content_block(prefix),
                        mcp_resized_image_content_block(blob, mime_type)?,
                    ]);
                }
                return Ok(persist_blob_to_text_block(
                    decode_mcp_base64(&blob),
                    mime_type,
                    server_name,
                    &prefix,
                ));
            }
            Ok(Vec::new())
        }
        "resource_link" => {
            let name = result_content
                .get("name")
                .map(js_string)
                .unwrap_or_default();
            let uri = result_content.get("uri").map(js_string).unwrap_or_default();
            let mut text = format!("[Resource link: {name}] {uri}");
            if let Some(description) = result_content.get("description") {
                text.push_str(&format!(" ({})", js_string(description)));
            }
            Ok(vec![mcp_text_content_block(text)])
        }
        _ => Ok(Vec::new()),
    }
}

/// Maps to: CC `services/mcp/client.ts` `transformMCPResult(...)`.
pub fn transform_mcp_result(
    result: &Value,
    tool: &str,
    name: &str,
) -> anyhow::Result<TransformedMcpResult> {
    if let Some(object) = result.as_object() {
        if let Some(tool_result) = object.get("toolResult") {
            return Ok(TransformedMcpResult {
                content: Value::String(js_string(tool_result)),
                result_type: crate::utils::mcp_output_storage::McpResultType::ToolResult,
                schema: None,
            });
        }

        if object.contains_key("structuredContent") {
            let structured_content = object
                .get("structuredContent")
                .expect("contains_key checked");
            return Ok(TransformedMcpResult {
                content: Value::String(json_stringify(structured_content)),
                result_type: crate::utils::mcp_output_storage::McpResultType::StructuredContent,
                schema: Some(infer_compact_schema(structured_content, 2)),
            });
        }

        if let Some(content) = object.get("content").and_then(Value::as_array) {
            let mut transformed_content = Vec::new();
            for item in content {
                transformed_content.extend(transform_result_content(item, name)?);
            }
            let transformed_value = Value::Array(transformed_content);
            return Ok(TransformedMcpResult {
                schema: Some(infer_compact_schema(&transformed_value, 2)),
                content: transformed_value,
                result_type: crate::utils::mcp_output_storage::McpResultType::ContentArray,
            });
        }
    }

    let message = format!("MCP server \"{name}\" tool \"{tool}\": unexpected response format");
    tracing::error!(server = %name, tool = %tool, "{message}");
    Err(anyhow::anyhow!(message))
}

/// Maps to: CC `processMCPResult(...)` output projection before MCP large-output
/// truncation/persistence. This keeps dynamic MCP tool execution using the same
/// result priority as `transformMCPResult`: `toolResult`, then
/// `structuredContent`, then transformed `content[]`.
pub fn transformed_mcp_result_summary(transformed: &TransformedMcpResult) -> String {
    match &transformed.content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| json_stringify(block))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => json_stringify(other),
    }
}

/// Maps to: CC `services/mcp/client.ts:2711-2718` `contentContainsImages`.
fn content_contains_images(content: &Value) -> bool {
    matches!(
        content,
        Value::Array(blocks) if blocks.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("image"))
    )
}

/// Maps to: CC `services/mcp/client.ts:2720-2798` `processMCPResult`.
pub async fn process_mcp_result(
    result: &Value,
    tool: &str,
    name: &str,
) -> anyhow::Result<TransformedMcpResult> {
    let mut transformed = transform_mcp_result(result, tool, name)?;
    if name == "ide" {
        return Ok(transformed);
    }

    if !crate::utils::mcp_validation::mcp_content_needs_truncation(&transformed.content).await {
        return Ok(transformed);
    }

    if crate::utils::env_utils::is_env_defined_falsy(
        crate::utils::process_env::var("ENABLE_MCP_LARGE_OUTPUT_FILES")
            .as_deref(),
    ) || content_contains_images(&transformed.content)
    {
        transformed.content =
            crate::utils::mcp_validation::truncate_mcp_content_if_needed(&transformed.content)
                .await;
        return Ok(transformed);
    }

    let persist_id = format!(
        "mcp-{}-{}-{}",
        crate::services::mcp::normalization::normalize_name_for_mcp(name),
        crate::services::mcp::normalization::normalize_name_for_mcp(tool),
        chrono::Utc::now().timestamp_millis()
    );
    let content_string = match &transformed.content {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| json_stringify(other)),
    };
    transformed.content = Value::String(
        match crate::utils::tool_result_storage::persist_tool_result_text(
            &content_string,
            &persist_id,
        ) {
            Ok(result) => {
                let format_description = crate::utils::mcp_output_storage::get_format_description(
                    transformed.result_type,
                    transformed.schema.as_deref(),
                );
                crate::utils::mcp_output_storage::get_large_output_instructions(
                    &result.filepath.to_string_lossy(),
                    result.original_size,
                    &format_description,
                    None,
                )
            }
            Err(error) => format!(
                "Error: result ({} characters) exceeds maximum allowed tokens. Failed to save output to file: {}. If this MCP server provides pagination or filtering tools, use them to retrieve specific portions of the data.",
                content_string.len(),
                error.error
            ),
        },
    );
    transformed.result_type = crate::utils::mcp_output_storage::McpResultType::ToolResult;
    transformed.schema = None;
    Ok(transformed)
}

/// Maps to: CC `MAX_MCP_DESCRIPTION_LENGTH` in `services/mcp/client.ts`.
pub const MAX_MCP_DESCRIPTION_LENGTH: usize = 2048;

fn truncate_mcp_description(description: Option<String>) -> String {
    let Some(description) = description else {
        return String::new();
    };
    if description.chars().count() > MAX_MCP_DESCRIPTION_LENGTH {
        format!(
            "{}… [truncated]",
            description
                .chars()
                .take(MAX_MCP_DESCRIPTION_LENGTH)
                .collect::<String>()
        )
    } else {
        description
    }
}

fn server_tools_are_model_visible(status: McpServerConnectionType) -> bool {
    matches!(
        status,
        McpServerConnectionType::Connected | McpServerConnectionType::NeedsAuth
    )
}

/// Maps to: CC `services/mcp/client.ts` `isIncludedMcpTool(...)`.
fn is_included_mcp_tool_full_name(full_tool_name: &str) -> bool {
    const ALLOWED_IDE_TOOLS: &[&str] = &["mcp__ide__executeCode", "mcp__ide__getDiagnostics"];
    !full_tool_name.starts_with("mcp__ide__") || ALLOWED_IDE_TOOLS.contains(&full_tool_name)
}

/// Projects only server-defined MCP tools. Resource helper tools are synced
/// separately from the server capability bit so zero-resource servers remain
/// capable. Callback-local helper injection belongs to discovery/reconnect,
/// not to this raw server-tool projection.
fn project_mcp_server_tools(
    clients: &[super::types::McpServerSnapshot],
) -> Vec<crate::types::tools::Tool> {
    clients
        .iter()
        .filter(|server| server_tools_are_model_visible(server.client.status))
        .flat_map(|server| {
            server.tools.iter().filter_map(move |tool| {
                let full_name = crate::services::mcp::mcp_string_utils::build_mcp_tool_name(
                    &server.client.name,
                    &tool.name,
                );
                is_included_mcp_tool_full_name(&full_name).then(|| {
                    // Maps to: CC `fetchToolsForClient(...)` local `skipPrefix`
                    // for SDK MCP servers gated by `CLAUDE_AGENT_SDK_MCP_NO_PREFIX`.
                    let skip_prefix = server
                        .config
                        .as_ref()
                        .is_some_and(|config| config.transport == Transport::Sdk)
                        && crate::utils::env_utils::is_env_truthy(
                            crate::utils::process_env::var("CLAUDE_AGENT_SDK_MCP_NO_PREFIX")
                                .as_deref(),
                        );
                    crate::types::tools::Tool {
                        name: if skip_prefix {
                            tool.name.clone()
                        } else {
                            full_name
                        },
                        description: truncate_mcp_description(tool.description.clone()),
                        input_schema: tool.input_schema.clone(),
                        is_mcp: true,
                        // Maps to: CC `client.ts:1774` — populated for both
                        // prefixed and skip-prefix tools, so permission rule
                        // matching can rebuild the qualified name either way.
                        mcp_info: Some(crate::types::tools::McpToolInfo {
                            server_name: server.client.name.clone(),
                            tool_name: tool.name.clone(),
                        }),
                        ..Default::default()
                    }
                })
            })
        })
        .collect()
}

/// Maps to: CC `options.hasPendingMCPServers` request option.
pub fn has_pending_mcp_servers(state: &McpState) -> bool {
    state
        .clients
        .iter()
        .any(|server| server.client.status == McpServerConnectionType::Pending)
}

/// Maps to: CC `fetchCommandsForClient(...)` MCP prompt command name:
/// `'mcp__' + normalizeNameForMCP(client.name) + '__' + prompt.name`.
pub fn mcp_prompt_command_name(server_name: &str, prompt_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        crate::services::mcp::normalization::normalize_name_for_mcp(server_name),
        prompt_name
    )
}

/// Maps to: CC `fetchCommandsForClient(...)` `userFacingName()` for MCP prompt
/// commands.
pub fn mcp_prompt_user_facing_name(server_name: &str, prompt_name: &str) -> String {
    format!("{server_name}:{prompt_name} (MCP)")
}

/// Maps to: CC `services/mcp/client.ts:1971-1975` — the `userFacingName()` that
/// `fetchToolsForClient(...)` hangs on every dynamic MCP tool:
///
/// ```ts
/// userFacingName() {
///   const displayName = tool.annotations?.title || tool.name
///   return `${client.name} - ${displayName} (MCP)`
/// }
/// ```
///
/// This had no Rust producer at all, so `extractMcpToolDisplayName`'s
/// `" - "`/`" (MCP)"` parsing (`mcp_string_utils.rs:61`) and the fallback
/// permission dialog's `" (MCP)"` strip branch
/// (`FallbackPermissionRequest.tsx:33-35`) were both unreachable.
pub fn mcp_tool_user_facing_name(server_name: &str, display_name: &str) -> String {
    format!("{server_name} - {display_name} (MCP)")
}

/// Maps to: CC `client.ts:1786-1788` MCP Tool's `description` callback.
/// This is the raw permission/UI description, unlike the separately
/// truncated `prompt` callback stored in the model-facing Tool descriptor.
pub fn mcp_tool_description(tool: &crate::services::mcp::types::McpToolSnapshot) -> String {
    tool.description.clone().unwrap_or_default()
}

/// Maps to: CC `client.ts:1973` `tool.annotations?.title || tool.name` — the
/// `||` is JS truthiness, so an empty title falls through to the tool name.
pub fn mcp_tool_display_name(annotation_title: Option<&str>, tool_name: &str) -> String {
    annotation_title
        .filter(|title| !title.is_empty())
        .unwrap_or(tool_name)
        .to_string()
}

/// Maps to: CC `fetchCommandsForClient(...)` `zipObject(argNames,
/// args.split(' '))` in `getPromptForCommand(args)`.
pub fn mcp_prompt_arguments_from_args(arg_names: &[String], args: &str) -> Map<String, Value> {
    let args_array = args.split(' ').collect::<Vec<_>>();
    arg_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                Value::String(
                    args_array
                        .get(index)
                        .copied()
                        .unwrap_or_default()
                        .to_string(),
                ),
            )
        })
        .collect()
}

/// Maps to: CC `fetchCommandsForClient(...)` command projection for MCP prompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpPromptCommandSnapshot {
    pub name: String,
    pub description: String,
    pub has_user_specified_description: bool,
    pub user_facing_name: String,
    pub arg_names: Vec<String>,
    pub source: &'static str,
}

fn project_mcp_prompt_commands_from_clients(
    clients: &[super::types::McpServerSnapshot],
) -> Vec<McpPromptCommandSnapshot> {
    clients
        .iter()
        .filter(|server| server.client.status == McpServerConnectionType::Connected)
        .flat_map(|server| {
            server.prompts.iter().map(move |prompt| {
                let description = prompt.description.clone().unwrap_or_default();
                McpPromptCommandSnapshot {
                    name: mcp_prompt_command_name(&server.client.name, &prompt.name),
                    description: description.clone(),
                    has_user_specified_description: !description.is_empty(),
                    user_facing_name: mcp_prompt_user_facing_name(
                        &server.client.name,
                        &prompt.name,
                    ),
                    arg_names: prompt.arg_names.clone(),
                    source: "mcp",
                }
            })
        })
        .collect()
}

/// Projects one connection update into the API-visible flat tool array.
pub(crate) fn mcp_tools_for_server_snapshot(
    server: &super::types::McpServerSnapshot,
) -> Vec<crate::types::tools::Tool> {
    project_mcp_server_tools(std::slice::from_ref(server))
}

/// Projects one connection update into the shared flat command protocol.
pub(crate) fn mcp_commands_for_server_snapshot(
    server: &super::types::McpServerSnapshot,
) -> Vec<crate::commands::Command> {
    project_mcp_prompt_commands_from_clients(std::slice::from_ref(server))
        .into_iter()
        .map(crate::commands::Command::from_mcp_prompt)
        .collect()
}

pub fn mcp_prompt_command_snapshot(
    server_name: &str,
    prompt: &McpPromptSnapshot,
) -> McpPromptCommandSnapshot {
    let description = prompt.description.clone().unwrap_or_default();
    McpPromptCommandSnapshot {
        name: mcp_prompt_command_name(server_name, &prompt.name),
        description: description.clone(),
        has_user_specified_description: !description.is_empty(),
        user_facing_name: mcp_prompt_user_facing_name(server_name, &prompt.name),
        arg_names: prompt.arg_names.clone(),
        source: "mcp",
    }
}

/// Resolves a slash-command-facing `mcp__server__prompt` command back to the
/// live raw MCP server/prompt names. Maps to CC `fetchCommandsForClient(...)`
/// prompt command records consumed by `processPromptSlashCommand(...)`.
pub fn resolve_mcp_prompt_command_invocation(
    full_command_name: &str,
    state: &McpState,
) -> Option<(String, McpPromptSnapshot)> {
    state
        .clients
        .iter()
        .filter(|server| server.client.status == McpServerConnectionType::Connected)
        .find_map(|server| {
            server.prompts.iter().find_map(|prompt| {
                let candidate = mcp_prompt_command_name(&server.client.name, &prompt.name);
                (candidate == full_command_name)
                    .then(|| (server.client.name.clone(), prompt.clone()))
            })
        })
}

/// Flattens MCP prompt content for the current transcript/model-message bridge.
/// Maps to CC `processPromptSlashCommand(...)` returning `ContentBlockParam[]`.
/// Rust currently stores prompt submissions as text-only `RenderableMessage` rows, so
/// non-text prompt blocks are preserved as JSON text until native block-carrying
/// transcript parity lands.
pub fn mcp_prompt_content_blocks_summary(blocks: &[Value]) -> String {
    blocks
        .iter()
        .map(|block| {
            block
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| block.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolves a model-facing `mcp__server__tool` name back to the live raw MCP
/// server/tool names. Maps to CC `buildMcpToolName(...)` + `mcpInfo` carried on
/// dynamic MCP tools from `fetchToolsForClient(...)`.
pub fn resolve_mcp_tool_invocation(
    full_tool_name: &str,
    state: &McpState,
) -> Option<(String, String)> {
    state
        .clients
        .iter()
        .filter(|server| server_tools_are_model_visible(server.client.status))
        .find_map(|server| {
            server.tools.iter().find_map(move |tool| {
                let full_name = crate::services::mcp::mcp_string_utils::build_mcp_tool_name(
                    &server.client.name,
                    &tool.name,
                );
                // Maps to: CC `fetchToolsForClient(...)` local `skipPrefix`
                // for SDK MCP servers gated by `CLAUDE_AGENT_SDK_MCP_NO_PREFIX`.
                let skip_prefix = server
                    .config
                    .as_ref()
                    .is_some_and(|config| config.transport == Transport::Sdk)
                    && crate::utils::env_utils::is_env_truthy(
                        crate::utils::process_env::var("CLAUDE_AGENT_SDK_MCP_NO_PREFIX")
                            .as_deref(),
                    );
                let candidate = if skip_prefix {
                    tool.name.clone()
                } else {
                    full_name.clone()
                };
                (candidate == full_tool_name && is_included_mcp_tool_full_name(&full_name))
                    .then(|| (server.client.name.clone(), tool.name.clone()))
            })
        })
}

/// Returns the live MCP annotations for a model-facing dynamic tool name.
/// Maps to the per-tool overrides created by CC `fetchToolsForClient(...)`.
pub fn mcp_tool_snapshot_for_invocation(
    full_tool_name: &str,
    state: &McpState,
) -> Option<McpToolSnapshot> {
    let (server_name, raw_tool_name) = resolve_mcp_tool_invocation(full_tool_name, state)?;
    state
        .clients
        .iter()
        .find(|server| server.client.name == server_name)
        .and_then(|server| server.tools.iter().find(|tool| tool.name == raw_tool_name))
        .cloned()
}

/// Maps to the callback result of getMcpToolsCommandsAndResources and
/// reconnectMcpServerImpl. Keep the actual capability payload separately from
/// the native client snapshot: resource helper tools are callback-local.
#[derive(Clone, Debug)]
pub struct McpConnectionDiscovery {
    pub server: McpServerSnapshot,
    pub tools: Vec<crate::types::tools::Tool>,
    pub commands: Vec<crate::commands::Command>,
    pub resources: Option<Vec<super::types::ServerResource>>,
}

impl From<McpServerSnapshot> for McpConnectionDiscovery {
    fn from(server: McpServerSnapshot) -> Self {
        Self {
            tools: mcp_tools_for_server_snapshot(&server),
            commands: mcp_commands_for_server_snapshot(&server),
            resources: (!server.resources.is_empty()).then(|| server.resources.clone()),
            server,
        }
    }
}

impl McpConnectionDiscovery {
    /// Maps to client.ts resourceTools arrays at startup and reconnection.
    fn append_resource_tools(&mut self) {
        self.tools
            .push(crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema());
        self.tools
            .push(crate::tools::read_mcp_resource_tool::read_mcp_resource_tool_schema());
    }

    // Native extraction of client.ts's per-invocation resourceToolsAdded branch.
    fn add_discovery_resource_tools(&mut self, added: &std::sync::atomic::AtomicBool) {
        if self.server.client.status == McpServerConnectionType::Connected
            && self.server.supports_resources
            && !added.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.append_resource_tools();
        }
    }

    fn add_reconnect_resource_tools(&mut self) {
        if self.server.client.status != McpServerConnectionType::Connected {
            self.tools.clear();
            self.commands.clear();
            self.resources = None;
            return;
        }
        if self.server.supports_resources
            && !self.tools.iter().any(|tool| {
                crate::types::tools::tool_matches_name(
                    tool,
                    crate::tools::list_mcp_resources_tool::prompt::LIST_MCP_RESOURCES_TOOL_NAME,
                ) || crate::types::tools::tool_matches_name(
                    tool,
                    crate::tools::read_mcp_resource_tool::prompt::READ_MCP_RESOURCE_TOOL_NAME,
                )
            })
        {
            self.append_resource_tools();
        }
    }
}

/// Maps to: CC `services/mcp/client.ts#setupSdkMcpClients` return object.
#[derive(Clone, Debug)]
pub struct SdkMcpClientsSetup {
    pub clients: Vec<McpServerSnapshot>,
    pub tools: Vec<crate::types::tools::Tool>,
}

impl McpConnectionDiscovery {
    pub fn failed(
        name: impl Into<String>,
        config: &ScopedMcpServerConfig,
        error: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self::from(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name,
                status: McpServerConnectionType::Failed,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: config.ide_name.clone(),
                server_version: None,
                error: Some(error.into()),
            },
            config: Some(config.clone()),
            supports_resources: false,
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: Vec::new(),
        })
    }

    pub fn pending(name: impl Into<String>) -> Self {
        let name = name.into();
        Self::from(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name,
                status: McpServerConnectionType::Pending,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: None,
                error: None,
            },
            config: None,
            supports_resources: false,
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: Vec::new(),
        })
    }

    /// Maps to: CC `useManageMCPConnections.ts` pending-client AppState rows.
    pub fn pending_with_config(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        let name = name.into();
        Self::from(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: name.clone(),
                status: McpServerConnectionType::Pending,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: config
                    .ide_name
                    .clone()
                    .or_else(|| (name == "ide").then(|| "IDE".to_string())),
                server_version: None,
                error: None,
            },
            config: Some(config.clone()),
            supports_resources: false,
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: Vec::new(),
        })
    }

    /// Maps to: CC `useManageMCPConnections.ts` disabled-client AppState rows.
    pub fn disabled(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        let name = name.into();
        Self::from(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: name.clone(),
                status: McpServerConnectionType::Disabled,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: config
                    .ide_name
                    .clone()
                    .or_else(|| (name == "ide").then(|| "IDE".to_string())),
                server_version: None,
                error: None,
            },
            config: Some(config.clone()),
            supports_resources: false,
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: Vec::new(),
        })
    }

    pub fn needs_auth(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        let name = name.into();
        Self::from(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: name.clone(),
                status: McpServerConnectionType::NeedsAuth,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: config.ide_name.clone(),
                server_version: None,
                error: None,
            },
            config: Some(config.clone()),
            supports_resources: false,
            tools: vec![
                crate::tools::mcp_auth_tool::create_mcp_auth_tool(&name, config).snapshot(),
            ],
            prompts: Vec::new(),
            resources: Vec::new(),
        })
    }
}

/// Maps to: CC `services/mcp/client.ts:2214-2225#processBatched`.
/// `pMap`'s returned array is discarded: each processor publishes its own
/// completion, and a finished item immediately releases its concurrency slot.
#[cfg(any(feature = "mcp_runtime", test))]
pub(super) async fn process_batched<T, F, Fut>(items: Vec<T>, concurrency: usize, processor: F)
where
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures::StreamExt;
    futures::stream::iter(items)
        .for_each_concurrent(concurrency.max(1), processor)
        .await;
}

#[cfg(feature = "mcp_runtime")]
mod runtime {
    use super::*;
    use crate::hooks::use_ide_selection::{
        IdeSelection, ide_selection_reset, is_ide_mcp_server_name,
    };
    use crate::services::mcp::sdk_control_transport::{
        SdkControlClientTransport, SendMcpMessageCallback,
    };
    use futures::StreamExt;
    use futures::stream::{self, BoxStream};
    use http::{HeaderName, HeaderValue};
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, ClientNotification,
        CustomNotification, ElicitRequestParams as RmcpElicitRequestParams,
        ElicitResult as RmcpElicitResult, ElicitationAction as RmcpElicitationAction,
        ElicitationCapability, ElicitationResponseNotificationParam, ErrorCode,
        GetPromptRequestParams, Implementation, ListRootsResult, Meta, ReadResourceRequestParams,
        Root, RootsCapabilities, ServerInfo,
    };
    use rmcp::service::{
        ClientInitializeError, NotificationContext, Peer, QuitReason, RequestContext,
        RunningService, RunningServiceCancellationToken, RxJsonRpcMessage, ServiceError,
        TxJsonRpcMessage,
    };
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransportConfig, StreamableHttpError,
    };
    use rmcp::transport::{
        DynamicTransportError, StreamableHttpClientTransport, TokioChildProcess,
        Transport as RmcpTransport,
    };
    use rmcp::{ErrorData as McpError, RoleClient, serve_client};
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::fmt;
    use std::future::Future;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, LazyLock, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::process::Command;
    use tokio::sync::{Mutex, oneshot};

    const DEFAULT_MCP_CONNECTION_TIMEOUT_MS: u64 = 30_000;
    const MCP_AUTH_CACHE_TTL_MS: u64 = 15 * 60 * 1000;
    const MAX_URL_ELICITATION_RETRIES: usize = 3;

    pub(super) static CONNECTED_CLIENTS: LazyLock<
        std::sync::Mutex<HashMap<String, ConnectedMcpClient>>,
    > = LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
    static PENDING_ELICITATION_RESPONSES: LazyLock<
        Mutex<HashMap<(String, String), oneshot::Sender<ElicitationResult>>>,
    > = LazyLock::new(|| Mutex::new(HashMap::new()));
    static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

    #[derive(Clone)]
    pub(super) struct ConnectedMcpClient {
        pub(super) on_close_enabled: bool,
        config: ScopedMcpServerConfig,
        peer: Peer<RoleClient>,
        cancellation_token: Arc<std::sync::Mutex<Option<RunningServiceCancellationToken>>>,
        connection_id: u64,
        /// Maps to: CC `useIdeSelection` registering `selection_changed`
        /// handlers on the live ide client — the REPL-owned selection sender,
        /// captured per connection (`Some` only for `name == "ide"`). Entry
        /// removal drops it (CC useIdeSelection.ts:148 "No cleanup needed").
        ide_selection_sink: Option<async_channel::Sender<IdeSelection>>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct McpAuthCacheEntry {
        timestamp: u64,
    }

    type McpAuthCacheData = BTreeMap<String, McpAuthCacheEntry>;

    #[derive(Clone, Debug)]
    struct CometixMcpClientHandler {
        server_name: String,
        cwd: PathBuf,
        /// Connection identity assigned by `replace_cached_client` once the
        /// connection is cached. Backs the ide `selection_changed`
        /// stale-identity guard (CC useIdeSelection.ts:115-117
        /// `currentIDERef.current !== ideClient → return`): notifications from
        /// a replaced/uncached connection never reach the REPL sink.
        connection_id: Arc<OnceLock<u64>>,
    }

    impl CometixMcpClientHandler {
        fn new(server_name: &str) -> Self {
            Self {
                server_name: server_name.to_string(),
                cwd: std::env::current_dir().unwrap_or_default(),
                connection_id: Arc::new(OnceLock::new()),
            }
        }
    }

    fn rmcp_elicitation_params_to_event_params(
        params: &RmcpElicitRequestParams,
    ) -> ElicitationRequestParams {
        match params {
            RmcpElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => ElicitationRequestParams::Form {
                message: message.clone(),
                requested_schema: serde_json::to_value(requested_schema)
                    .unwrap_or_else(|_| Value::Object(Map::new())),
            },
            RmcpElicitRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => ElicitationRequestParams::Url {
                message: message.clone(),
                url: url.clone(),
                elicitation_id: Some(elicitation_id.clone()),
            },
            _ => ElicitationRequestParams::Form {
                // Safe fallback for future rmcp variants; current MCP variants
                // map above and match CC `getElicitationMode(...)`.
                message: "Unsupported MCP elicitation request".to_string(),
                requested_schema: Value::Object(Map::new()),
            },
        }
    }

    fn elicitation_result_to_rmcp(result: ElicitationResult) -> RmcpElicitResult {
        let action = match result.action {
            ElicitationAction::Accept => RmcpElicitationAction::Accept,
            ElicitationAction::Decline => RmcpElicitationAction::Decline,
            ElicitationAction::Cancel => RmcpElicitationAction::Cancel,
        };
        let mut out = RmcpElicitResult::new(action);
        if let Some(content) = result.content {
            out.content = Some(content);
        }
        out
    }

    fn cancel_elicitation_result() -> RmcpElicitResult {
        RmcpElicitResult::new(RmcpElicitationAction::Cancel)
    }

    fn rmcp_elicitation_hook_details(
        params: &RmcpElicitRequestParams,
    ) -> (
        String,
        Option<Value>,
        &'static str,
        Option<String>,
        Option<String>,
    ) {
        match params {
            RmcpElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => (
                message.clone(),
                Some(
                    serde_json::to_value(requested_schema)
                        .unwrap_or_else(|_| Value::Object(Map::new())),
                ),
                "form",
                None,
                None,
            ),
            RmcpElicitRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => (
                message.clone(),
                None,
                "url",
                Some(url.clone()),
                Some(elicitation_id.clone()),
            ),
            _ => (
                "Unsupported MCP elicitation request".to_string(),
                None,
                "form",
                None,
                None,
            ),
        }
    }

    fn load_mcp_elicitation_hooks_config_and_env() -> Option<(
        crate::services::hooks::RegisteredHooks,
        crate::services::hooks::HookContext,
        Vec<(String, String)>,
    )> {
        let loaded_hooks =
            crate::utils::hooks::hooks_config_snapshot::load_hooks_config_from_settings_sources();
        let config = loaded_hooks.config;
        if config.is_empty() {
            return None;
        }
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        let hook_context = crate::services::hooks::HookContext {
            cwd,
            permission_mode: loaded_hooks.merged_settings.default_permission_mode,
            ..Default::default()
        };
        Some((config, hook_context, Vec::new()))
    }

    async fn run_elicitation_hooks_for_request(
        server_name: &str,
        request: &RmcpElicitRequestParams,
    ) -> Option<ElicitationResult> {
        let (config, hook_context, base_env) = load_mcp_elicitation_hooks_config_and_env()?;
        let (message, requested_schema, mode, url, elicitation_id) =
            rmcp_elicitation_hook_details(request);
        let results = crate::services::hooks::elicitation::execute_elicitation_hooks(
            &config,
            &hook_context,
            server_name,
            &message,
            requested_schema,
            Some(mode),
            url.as_deref(),
            elicitation_id.as_deref(),
            base_env,
        )
        .await;
        let (response, blocking_error) = reduce_elicitation_hook_results(&results);
        if blocking_error.is_some() {
            Some(ElicitationResult::new(ElicitationAction::Decline))
        } else {
            response
        }
    }

    pub(super) fn elicitation_complete_notification_message(
        server_name: &str,
        elicitation_id: &str,
    ) -> String {
        // Maps to: CC `registerElicitationHandler(...)` completion
        // notification hook message.
        format!("MCP server \"{server_name}\" confirmed elicitation {elicitation_id} complete")
    }

    pub(super) fn elicitation_response_notification_message(
        server_name: &str,
        action: &str,
    ) -> String {
        // Maps to: CC `runElicitationResultHooks(...)` response
        // notification hook message.
        format!("Elicitation response for server \"{server_name}\": {action}")
    }

    fn spawn_elicitation_notification_hooks(message: String, notification_type: &'static str) {
        let Some((config, _, base_env)) = load_mcp_elicitation_hooks_config_and_env() else {
            return;
        };
        tokio::spawn(async move {
            crate::services::hooks::lifecycle::execute_notification_hooks(
                &config,
                &message,
                notification_type,
                None,
                base_env,
            )
            .await;
        });
    }

    async fn run_elicitation_result_hooks_for_response(
        server_name: &str,
        request: &RmcpElicitRequestParams,
        result: ElicitationResult,
    ) -> ElicitationResult {
        let Some((config, hook_context, base_env)) = load_mcp_elicitation_hooks_config_and_env()
        else {
            return result;
        };
        let (_, _, mode, _, elicitation_id) = rmcp_elicitation_hook_details(request);
        let results = crate::services::hooks::elicitation::execute_elicitation_result_hooks(
            &config,
            &hook_context,
            server_name,
            result.action.as_str(),
            result.content.clone(),
            Some(mode),
            elicitation_id.as_deref(),
            base_env,
        )
        .await;
        let final_result = reduce_elicitation_result_hook_results(&result, &results).0;
        spawn_elicitation_notification_hooks(
            elicitation_response_notification_message(server_name, final_result.action.as_str()),
            "elicitation_response",
        );
        final_result
    }

    fn server_info_has_experimental_capability(
        peer_info: Option<&ServerInfo>,
        capability: &str,
    ) -> bool {
        peer_info
            .and_then(|info| info.capabilities.experimental.as_ref())
            .is_some_and(|experimental| experimental.contains_key(capability))
    }

    async fn connected_client_plugin_source(server_name: &str) -> Option<String> {
        CONNECTED_CLIENTS
            .lock()
            .unwrap()
            .get(server_name)
            .and_then(|client| client.config.plugin_source.clone())
    }

    pub(super) async fn emit_ide_selection_event_from_custom_notification(
        server_name: &str,
        notification: &CustomNotification,
        notifying_connection_id: Option<u64>,
    ) -> bool {
        // Maps to: CC `useIdeSelection` `SelectionChangedSchema` handler.
        if !is_ide_mcp_server_name(server_name) {
            return false;
        }
        if notification.method != crate::hooks::use_ide_selection::SELECTION_CHANGED_METHOD {
            return false;
        }
        // Invalid / text-only payloads are consumed without an onSelect call
        // (CC useIdeSelection.ts:124-138 gate).
        let Some(selection) =
            crate::hooks::use_ide_selection::ide_selection_from_notification_params(
                notification.params.as_ref(),
            )
        else {
            return true;
        };
        let sink = {
            let clients = CONNECTED_CLIENTS.lock().unwrap();
            clients.get(server_name).and_then(|client| {
                // Maps to: CC useIdeSelection.ts:115-117 stale-identity guard —
                // only the currently cached connection may deliver selections.
                (Some(client.connection_id) == notifying_connection_id)
                    .then(|| client.ide_selection_sink.clone())
                    .flatten()
            })
        };
        if let Some(sink) = sink {
            // Maps to: CC `onSelect(selection)` — REPL's use_future drains this.
            let _ = sink.try_send(selection);
        }
        true
    }

    pub(super) async fn emit_channel_message_event_from_custom_notification(
        server_name: String,
        notification: CustomNotification,
        peer_info: Option<&ServerInfo>,
    ) -> bool {
        // Maps to: CC `ChannelMessageNotificationSchema` handler registered in
        // `useManageMCPConnections.ts` for `notifications/claude/channel`.
        if notification.method
            != crate::services::mcp::channel_notification::CHANNEL_MESSAGE_NOTIFICATION_METHOD
        {
            return false;
        }

        let parsed = match crate::services::mcp::channel_notification::parse_channel_message_notification_params(
            notification.params.as_ref(),
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(server = %server_name, error = %error, "invalid MCP channel notification");
                return false;
            }
        };
        let has_channel_capability = server_info_has_experimental_capability(
            peer_info,
            crate::services::mcp::channel_notification::CHANNEL_EXPERIMENTAL_CAPABILITY,
        );
        let plugin_source = connected_client_plugin_source(&server_name).await;
        crate::services::mcp::use_manage_mcp_connections::emit_channel_message_received(
            server_name,
            parsed.content,
            parsed.meta,
            has_channel_capability,
            plugin_source,
        )
        .await;
        true
    }

    pub(super) async fn emit_channel_permission_event_from_custom_notification(
        server_name: String,
        notification: CustomNotification,
        peer_info: Option<&ServerInfo>,
    ) -> bool {
        // Maps to: CC `ChannelPermissionNotificationSchema` handler registered
        // in `useManageMCPConnections.ts` for
        // `notifications/claude/channel/permission`.
        if notification.method
            != crate::services::mcp::channel_notification::CHANNEL_PERMISSION_METHOD
        {
            return false;
        }

        let parsed = match crate::services::mcp::channel_notification::parse_channel_permission_notification_params(
            notification.params.as_ref(),
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(server = %server_name, error = %error, "invalid MCP channel permission notification");
                return false;
            }
        };
        let has_permission_capability = server_info_has_experimental_capability(
            peer_info,
            crate::services::mcp::channel_notification::CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY,
        );
        crate::services::mcp::use_manage_mcp_connections::emit_channel_permission_received(
            server_name,
            parsed.request_id,
            parsed.behavior,
            has_permission_capability,
        )
        .await;
        true
    }

    pub async fn send_channel_permission_request_to_relays(
        params: &crate::services::mcp::channel_permissions::ChannelPermissionRequestParams,
    ) -> crate::services::mcp::channel_permissions::ChannelPermissionRelaySendReport {
        // Maps to: CC `interactiveHandler.ts` channel permission relay block.
        let mut report =
            crate::services::mcp::channel_permissions::ChannelPermissionRelaySendReport {
                enabled:
                    crate::services::mcp::channel_permissions::is_channel_permission_relay_enabled(),
                ..Default::default()
            };
        if !report.enabled {
            return report;
        }

        let allowed_channels = crate::bootstrap::state::get_allowed_channels();
        let targets = {
            let clients = CONNECTED_CLIENTS.lock().unwrap();
            // CC filters `AppState.mcp.clients` directly, because its
            // connection records carry `capabilities`. Rust keeps capabilities
            // on the live peer instead (see `McpServerSnapshot`'s doc), so the
            // registry is projected into the candidate shape first and the
            // experimental map is read from `peer_info`. The selection rule
            // itself is NOT reimplemented here — it belongs to
            // `channel_permissions`, which is what CC's
            // `filterPermissionRelayClients` maps to.
            let candidates = clients
                .iter()
                .map(|(name, client)| {
                    let experimental_capabilities: std::collections::BTreeMap<
                        String,
                        serde_json::Value,
                    > = client
                        .peer
                        .peer_info()
                        .and_then(|info| info.capabilities.experimental.clone())
                        .map(|map| {
                            // rmcp types each experimental entry as a JSON
                            // object; CC only ever tests key presence
                            // (`experimental['claude/channel'] !== undefined`),
                            // so the value is carried as-is.
                            map.into_iter()
                                .map(|(key, value)| (key, serde_json::Value::Object(value)))
                                .collect()
                        })
                        .unwrap_or_default();
                    crate::services::mcp::channel_permissions::ChannelPermissionRelayClientCandidate {
                        // Every entry in this registry is a live connection;
                        // CC's `c.type === 'connected'` clause is satisfied by
                        // construction.
                        client_type: "connected".to_string(),
                        name: name.clone(),
                        experimental_capabilities,
                    }
                })
                .collect::<Vec<_>>();
            crate::services::mcp::channel_permissions::filter_permission_relay_clients(
                &candidates,
                |name| {
                    crate::services::mcp::channel_notification::find_channel_entry(
                        name,
                        &allowed_channels,
                    )
                    .is_some()
                },
            )
            .into_iter()
            .filter_map(|candidate| {
                clients
                    .get(&candidate.name)
                    .map(|client| (candidate.name.clone(), client.peer.clone()))
            })
            .collect::<Vec<_>>()
        };

        let params_value = match serde_json::to_value(params) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "failed to serialize MCP channel permission request");
                return report;
            }
        };
        for (name, peer) in targets {
            report.attempted += 1;
            let notification = CustomNotification::new(
                crate::services::mcp::channel_notification::CHANNEL_PERMISSION_REQUEST_METHOD,
                Some(params_value.clone()),
            );
            match peer
                .send_notification(ClientNotification::CustomNotification(notification))
                .await
            {
                Ok(()) => report.sent += 1,
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(server = %name, error = %error, "MCP channel permission_request failed");
                }
            }
        }
        report
    }

    impl ClientHandler for CometixMcpClientHandler {
        fn get_info(&self) -> ClientInfo {
            // Maps to: CC `new Client({ name:'claude-code', title:'Claude Code', ... },
            // { capabilities:{ roots:{}, elicitation:{} } })`.
            let mut implementation = Implementation::new(
                "claude-code",
                option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"),
            );
            implementation.title = Some("Claude Code".to_string());
            implementation.description = Some("Anthropic's agentic coding tool".to_string());
            implementation.website_url = Some(crate::constants::product::PRODUCT_URL.to_string());

            let mut capabilities = ClientCapabilities::default();
            capabilities.roots = Some(RootsCapabilities::default());
            // CC intentionally sends an empty elicitation object, not
            // form/url subcapabilities, for Java MCP SDK compatibility.
            capabilities.elicitation = Some(ElicitationCapability::default());

            ClientInfo::new(capabilities, implementation)
        }

        fn list_roots(
            &self,
            _context: RequestContext<RoleClient>,
        ) -> impl Future<Output = Result<ListRootsResult, McpError>> + Send + '_ {
            let cwd = self.cwd.clone();
            async move {
                #[allow(deprecated)]
                Ok(ListRootsResult::new(vec![Root::new(format!(
                    "file://{}",
                    cwd.to_string_lossy()
                ))]))
            }
        }

        fn create_elicitation(
            &self,
            request: RmcpElicitRequestParams,
            context: RequestContext<RoleClient>,
        ) -> impl Future<Output = Result<RmcpElicitResult, McpError>> + Send + '_ {
            let server_name = self.server_name.clone();
            async move {
                // Maps to: CC `registerElicitationHandler(...)`: hooks can
                // resolve the request before REPL UI is queued.
                if let Some(hook_response) =
                    run_elicitation_hooks_for_request(&server_name, &request).await
                {
                    return Ok(elicitation_result_to_rmcp(hook_response));
                }

                // Maps to: CC `registerElicitationHandler(...)` queuing an
                // `ElicitationRequestEvent` in `AppState.elicitation.queue`.
                let request_id = context.id.to_string();
                let (sender, receiver) = oneshot::channel::<ElicitationResult>();
                PENDING_ELICITATION_RESPONSES
                    .lock()
                    .await
                    .insert((server_name.clone(), request_id.clone()), sender);
                let event = ElicitationRequestEvent::new(
                    server_name.clone(),
                    request_id.clone(),
                    rmcp_elicitation_params_to_event_params(&request),
                );
                crate::services::mcp::use_manage_mcp_connections::emit_elicitation_requested(event)
                    .await;

                let raw_result = tokio::select! {
                    _ = context.ct.cancelled() => {
                        PENDING_ELICITATION_RESPONSES
                            .lock()
                            .await
                            .remove(&(server_name.clone(), request_id));
                        ElicitationResult::new(ElicitationAction::Cancel)
                    }
                    response = receiver => {
                        response.unwrap_or_else(|_| ElicitationResult::new(ElicitationAction::Cancel))
                    }
                };
                let final_result =
                    run_elicitation_result_hooks_for_response(&server_name, &request, raw_result)
                        .await;
                Ok(elicitation_result_to_rmcp(final_result))
            }
        }

        fn on_url_elicitation_notification_complete(
            &self,
            params: ElicitationResponseNotificationParam,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            async move {
                // Maps to: CC `ElicitationCompleteNotificationSchema` handler.
                spawn_elicitation_notification_hooks(
                    elicitation_complete_notification_message(&server_name, &params.elicitation_id),
                    "elicitation_complete",
                );
                crate::services::mcp::use_manage_mcp_connections::emit_elicitation_completed(
                    server_name,
                    params.elicitation_id,
                )
                .await;
            }
        }

        fn on_tool_list_changed(
            &self,
            context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            // The capability gate itself belongs to `use_manage_mcp_connections`
            // (CC `useManageMCPConnections.ts:618`); this handler only supplies
            // the peer info, which is the transport's to know.
            let declared = context.peer.peer_info().is_some_and(|info| {
                crate::services::mcp::use_manage_mcp_connections::declares_tools_list_changed(
                    &info.capabilities,
                )
            });
            async move {
                if !declared {
                    tracing::debug!(server = %server_name, "ignoring tools/list_changed from a server that did not declare tools.listChanged");
                    return;
                }
                // Maps to: CC `ToolListChangedNotificationSchema` handler in
                // `useManageMCPConnections.ts`.
                tracing::debug!(server = %server_name, "received MCP tools/list_changed notification");
                crate::services::mcp::use_manage_mcp_connections::emit_tools_list_changed(
                    server_name,
                )
                .await;
            }
        }

        fn on_prompt_list_changed(
            &self,
            context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            // Gate owned by `use_manage_mcp_connections` (CC `:667`).
            let declared = context.peer.peer_info().is_some_and(|info| {
                crate::services::mcp::use_manage_mcp_connections::declares_prompts_list_changed(
                    &info.capabilities,
                )
            });
            async move {
                if !declared {
                    tracing::debug!(server = %server_name, "ignoring prompts/list_changed from a server that did not declare prompts.listChanged");
                    return;
                }
                // Maps to: CC `PromptListChangedNotificationSchema` handler.
                tracing::debug!(server = %server_name, "received MCP prompts/list_changed notification");
                crate::services::mcp::use_manage_mcp_connections::emit_prompts_list_changed(
                    server_name,
                )
                .await;
            }
        }

        fn on_resource_list_changed(
            &self,
            context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            // Gate owned by `use_manage_mcp_connections` (CC `:705`).
            let declared = context.peer.peer_info().is_some_and(|info| {
                crate::services::mcp::use_manage_mcp_connections::declares_resources_list_changed(
                    &info.capabilities,
                )
            });
            async move {
                if !declared {
                    tracing::debug!(server = %server_name, "ignoring resources/list_changed from a server that did not declare resources.listChanged");
                    return;
                }
                // Maps to: CC `ResourceListChangedNotificationSchema` handler.
                tracing::debug!(server = %server_name, "received MCP resources/list_changed notification");
                crate::services::mcp::use_manage_mcp_connections::emit_resources_list_changed(
                    server_name,
                )
                .await;
            }
        }

        fn on_custom_notification(
            &self,
            notification: CustomNotification,
            context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            let notifying_connection_id = self.connection_id.get().copied();
            async move {
                if crate::services::mcp::vscode_sdk_mcp::handle_vscode_log_event_notification(
                    &server_name,
                    &notification,
                ) {
                    return;
                }
                if emit_ide_selection_event_from_custom_notification(
                    &server_name,
                    &notification,
                    notifying_connection_id,
                )
                .await
                {
                    return;
                }
                let peer_info = context.peer.peer_info();
                if emit_channel_message_event_from_custom_notification(
                    server_name.clone(),
                    notification.clone(),
                    peer_info.as_deref(),
                )
                .await
                {
                    return;
                }
                let _ = emit_channel_permission_event_from_custom_notification(
                    server_name,
                    notification,
                    peer_info.as_deref(),
                )
                .await;
            }
        }
    }

    #[derive(Debug)]
    struct McpTransportIoError(anyhow::Error);

    impl fmt::Display for McpTransportIoError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for McpTransportIoError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.0.source()
        }
    }

    impl From<anyhow::Error> for McpTransportIoError {
        fn from(error: anyhow::Error) -> Self {
            Self(error)
        }
    }

    #[derive(Debug)]
    pub(super) struct McpRemoteAuthHttpError {
        pub(super) status: u16,
        pub(super) www_authenticate: Option<String>,
        pub(super) body: String,
    }

    impl fmt::Display for McpRemoteAuthHttpError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.www_authenticate.as_deref() {
                Some(challenge) => write!(
                    f,
                    "remote MCP auth HTTP {}: WWW-Authenticate: {}; {}",
                    self.status, challenge, self.body
                ),
                None => write!(f, "remote MCP auth HTTP {}: {}", self.status, self.body),
            }
        }
    }

    impl std::error::Error for McpRemoteAuthHttpError {}

    async fn ensure_success_or_auth_error(
        response: reqwest::Response,
    ) -> anyhow::Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let www_authenticate = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read response body>".to_string());
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(anyhow::Error::new(McpRemoteAuthHttpError {
                status: status.as_u16(),
                www_authenticate,
                body,
            }));
        }
        Err(anyhow::anyhow!("HTTP {status}: {body}"))
    }

    struct LegacySseTransport {
        client: reqwest::Client,
        post_url: String,
        post_headers: BTreeMap<String, String>,
        incoming: BoxStream<'static, RxJsonRpcMessage<RoleClient>>,
    }

    impl RmcpTransport<RoleClient> for LegacySseTransport {
        type Error = McpTransportIoError;

        fn send(
            &mut self,
            item: TxJsonRpcMessage<RoleClient>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            let client = self.client.clone();
            let post_url = self.post_url.clone();
            let post_headers = self.post_headers.clone();
            async move {
                // Maps to: CC `SSEClientTransport.send(...)` POST to the
                // endpoint advertised by the server's initial SSE event.
                let mut request = client.post(&post_url).json(&item);
                for (key, value) in post_headers {
                    request = request.header(&key, value);
                }
                let response = request
                    .send()
                    .await
                    .map_err(|error| McpTransportIoError(anyhow::Error::new(error)))?;
                ensure_success_or_auth_error(response)
                    .await
                    .map_err(McpTransportIoError)?;
                Ok(())
            }
        }

        fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
            self.incoming.next()
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn parse_mcp_rx_message(
        server_name: &str,
        payload: &str,
    ) -> Option<RxJsonRpcMessage<RoleClient>> {
        match serde_json::from_str::<RxJsonRpcMessage<RoleClient>>(payload) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::warn!(server = server_name, error = %error, "failed to parse MCP transport message");
                None
            }
        }
    }

    fn resolve_legacy_sse_endpoint(base_url: &str, endpoint: &str) -> anyhow::Result<String> {
        // Maps to: CC SDK `SSEClientTransport` endpoint resolution.
        if endpoint.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "SSE MCP server did not advertise a POST endpoint"
            ));
        }
        match reqwest::Url::parse(endpoint) {
            Ok(url) => Ok(url.to_string()),
            Err(_) => Ok(reqwest::Url::parse(base_url)?.join(endpoint)?.to_string()),
        }
    }

    async fn legacy_sse_transport(
        name: &str,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> anyhow::Result<LegacySseTransport> {
        // Maps to: CC `new SSEClientTransport(new URL(serverRef.url), ...)`.
        // Its event stream spreads `getProxyFetchOptions()` (`client.ts:657-669`)
        // and its POSTs take the global `fetch`: the same transport.
        let client = crate::utils::proxy::get_proxy_fetch_options(false)?.build()?;
        let mut request = client.get(url).header("Accept", "text/event-stream");
        for (key, value) in &headers {
            request = request.header(key, value);
        }
        let response = ensure_success_or_auth_error(request.send().await?).await?;
        let mut sse_stream = sse_stream::SseStream::from_byte_stream(response.bytes_stream());
        let mut initial_messages = Vec::new();
        let post_url = loop {
            let Some(event) = sse_stream.next().await else {
                return Err(anyhow::anyhow!(
                    "SSE MCP server closed before advertising endpoint"
                ));
            };
            let event = event.map_err(|error| anyhow::anyhow!(error))?;
            let event_name = event.event.as_deref().unwrap_or("message");
            match event_name {
                "endpoint" => {
                    let endpoint = event.data.as_deref().unwrap_or_default();
                    break resolve_legacy_sse_endpoint(url, endpoint)?;
                }
                "message" => {
                    if let Some(data) = event.data.as_deref() {
                        if let Some(message) = parse_mcp_rx_message(name, data) {
                            initial_messages.push(message);
                        }
                    }
                }
                _ => {}
            }
        };

        let server_name = name.to_string();
        let incoming_tail = sse_stream.filter_map(move |event| {
            let server_name = server_name.clone();
            async move {
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(server = %server_name, error = %error, "legacy SSE MCP stream error");
                        return None;
                    }
                };
                let event_name = event.event.as_deref().unwrap_or("message");
                if event_name != "message" {
                    return None;
                }
                let payload = event.data.as_deref()?;
                parse_mcp_rx_message(&server_name, payload)
            }
        });

        Ok(LegacySseTransport {
            client,
            post_url,
            post_headers: headers,
            incoming: stream::iter(initial_messages).chain(incoming_tail).boxed(),
        })
    }

    fn connection_timeout_ms() -> u64 {
        crate::utils::process_env::var("MCP_TIMEOUT")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MCP_CONNECTION_TIMEOUT_MS)
    }

    /// Maps to: CC `services/mcp/client.ts:552-554`
    /// `getMcpServerConnectionBatchSize`.
    pub(super) fn get_mcp_server_connection_batch_size() -> usize {
        crate::utils::process_env::var("MCP_SERVER_CONNECTION_BATCH_SIZE")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(3)
    }

    /// Maps to: CC `services/mcp/client.ts:556-561`
    /// `getRemoteMcpServerConnectionBatchSize`.
    pub(super) fn get_remote_mcp_server_connection_batch_size() -> usize {
        crate::utils::process_env::var("MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(20)
    }

    fn is_local_mcp_server(config: &ScopedMcpServerConfig) -> bool {
        // Maps to: CC `services/mcp/client.ts#isLocalMcpServer`.
        matches!(config.transport, Transport::Stdio | Transport::Sdk)
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default()
    }

    fn mcp_auth_cache_path() -> PathBuf {
        // Maps to: CC `getMcpAuthCachePath()`.
        crate::utils::env_utils::get_claude_config_home_dir().join("mcp-needs-auth-cache.json")
    }

    fn remote_transport_uses_auth_cache(transport: Transport) -> bool {
        matches!(
            transport,
            Transport::Sse | Transport::Http | Transport::ClaudeAiProxy
        )
    }

    fn read_mcp_auth_cache() -> McpAuthCacheData {
        std::fs::read_to_string(mcp_auth_cache_path())
            .ok()
            .and_then(|content| serde_json::from_str::<McpAuthCacheData>(&content).ok())
            .unwrap_or_default()
    }

    fn is_mcp_auth_cached(server_id: &str) -> bool {
        // Maps to: CC `isMcpAuthCached(serverId)` 15-minute TTL.
        let cache = read_mcp_auth_cache();
        let Some(entry) = cache.get(server_id) else {
            return false;
        };
        now_ms().saturating_sub(entry.timestamp) < MCP_AUTH_CACHE_TTL_MS
    }

    fn set_mcp_auth_cache_entry(server_id: &str) {
        // Maps to: CC `setMcpAuthCacheEntry(serverId)`.
        let mut cache = read_mcp_auth_cache();
        cache.insert(
            server_id.to_string(),
            McpAuthCacheEntry {
                timestamp: now_ms(),
            },
        );
        let path = mcp_auth_cache_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&cache) {
            Ok(content) => {
                if let Err(error) = std::fs::write(&path, content) {
                    tracing::warn!(path = %path.display(), error = %error, "failed to write MCP auth cache");
                }
            }
            Err(error) => tracing::warn!(error = %error, "failed to serialize MCP auth cache"),
        }
    }

    pub fn clear_mcp_auth_cache() {
        // Maps to: CC `clearMcpAuthCache()`.
        let _ = std::fs::remove_file(mcp_auth_cache_path());
    }

    fn stdio_command(config: &ScopedMcpServerConfig) -> anyhow::Result<Command> {
        let command = config
            .command
            .as_deref()
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("stdio MCP server is missing command"))?;
        let mut cmd = Command::new(command);
        cmd.args(&config.args);
        // CC client.ts:953-956: `{ ...subprocessEnv(), ...serverRef.env }`.
        crate::utils::subprocess_env::apply_subprocess_env(&mut cmd);
        cmd.envs(&config.env);
        cmd.stderr(Stdio::piped());
        Ok(cmd)
    }

    async fn serve_stdio(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let (transport, _stderr) = TokioChildProcess::builder(stdio_command(config)?)
            .stderr(Stdio::piped())
            .spawn()?;
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    fn session_ingress_auth() -> (Option<String>, BTreeMap<String, String>) {
        // Maps to: CC `utils/sessionIngressAuth.ts#getSessionIngressAuthToken`
        // and `getSessionIngressAuthHeaders`.
        let Some(token) = crate::utils::session_ingress_auth::get_session_ingress_auth_token()
        else {
            return (None, BTreeMap::new());
        };
        if token.starts_with("sk-ant-sid") {
            (
                None,
                crate::utils::session_ingress_auth::get_session_ingress_auth_headers(),
            )
        } else {
            (Some(token), BTreeMap::new())
        }
    }

    pub(super) fn streamable_http_auth_header(
        oauth_token: Option<String>,
        session_bearer: Option<String>,
        has_static_authorization: bool,
    ) -> Option<String> {
        // Maps to: CC HTTP MCP request header precedence in
        // `connectToServer`: OAuth provider wins; otherwise session ingress
        // bearer is attached when no static Authorization header overrides it.
        if oauth_token.is_some() {
            oauth_token
        } else if !has_static_authorization {
            session_bearer
        } else {
            None
        }
    }

    fn header_map_from_strings(
        headers: BTreeMap<String, String>,
    ) -> anyhow::Result<HashMap<HeaderName, HeaderValue>> {
        let mut result = HashMap::new();
        for (key, value) in headers {
            let name = HeaderName::from_str(&key)
                .map_err(|error| anyhow::anyhow!("invalid MCP header name '{key}': {error}"))?;
            let value = HeaderValue::from_str(&value).map_err(|error| {
                anyhow::anyhow!("invalid MCP header value for '{key}': {error}")
            })?;
            result.insert(name, value);
        }
        Ok(result)
    }

    async fn streamable_http_config(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<StreamableHttpClientTransportConfig> {
        // Maps to: CC `StreamableHTTPClientTransportOptions.requestInit.headers`.
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        headers.extend(
            crate::services::mcp::headers_helper::get_mcp_server_headers(name, config).await,
        );

        let oauth_token = crate::services::mcp::auth::ClaudeAuthProvider::new(name, config)
            .tokens()?
            .and_then(|tokens| tokens.access_token)
            .filter(|token| !token.is_empty());
        let (session_bearer, session_headers) = session_ingress_auth();
        let has_static_authorization = headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"));
        let auth_header =
            streamable_http_auth_header(oauth_token, session_bearer, has_static_authorization);
        if auth_header.is_none() && !has_static_authorization {
            headers.extend(session_headers);
        }

        Ok(
            StreamableHttpClientTransportConfig::with_uri(url.to_string())
                .custom_headers(header_map_from_strings(headers)?)
                .reinit_on_expired_session(true)
                .auth_header(auth_header.unwrap_or_default()),
        )
    }

    fn claude_ai_proxy_http_config(
        url: &str,
        token: String,
    ) -> anyhow::Result<StreamableHttpClientTransportConfig> {
        // Maps to: CC `claudeai-proxy` StreamableHTTP options.
        let headers = BTreeMap::from([
            (
                "User-Agent".to_string(),
                crate::utils::http::get_mcp_user_agent(),
            ),
            (
                "X-Mcp-Client-Session-Id".to_string(),
                crate::bootstrap::state::get_session_id(),
            ),
        ]);
        Ok(
            StreamableHttpClientTransportConfig::with_uri(url.to_string())
                .custom_headers(header_map_from_strings(headers)?)
                .reinit_on_expired_session(true)
                .auth_header(token),
        )
    }

    async fn serve_streamable_http(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let mut transport_config = streamable_http_config(name, config, url).await?;
        if transport_config.auth_header.as_deref() == Some("") {
            transport_config.auth_header = None;
        }
        let transport = StreamableHttpClientTransport::with_client(
            streamable_http_client()?,
            transport_config,
        );
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    /// The client rmcp's Streamable HTTP transport sends with. CC spreads
    /// `getProxyFetchOptions()` into the transport's `requestInit`
    /// (`client.ts:815-831`, `:887`). rmcp's own default has no idle pool and
    /// follows no redirect (`default_http_client`); both are kept.
    pub(super) fn streamable_http_client() -> anyhow::Result<reqwest::Client> {
        Ok(crate::utils::proxy::get_proxy_fetch_options(false)?
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }

    async fn serve_claude_ai_proxy_http_with_token(
        name: &str,
        proxy_url: &str,
        token: String,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        // Rust/rmcp transport boundary for the source's nested `doRequest`:
        // each retry must start a new service with the bearer token it sends.
        let transport_config = claude_ai_proxy_http_config(proxy_url, token)?;
        let transport = StreamableHttpClientTransport::with_client(
            streamable_http_client()?,
            transport_config,
        );
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    async fn serve_claude_ai_proxy_http(
        name: &str,
        proxy_url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        // Maps to: CC `services/mcp/client.ts:369-424`
        // `createClaudeAiProxyFetch`: refresh before attaching the bearer token
        // and retry once after a 401 only when the failed token changed or a
        // force-refresh succeeded.
        let read_access_token =
            || crate::utils::auth::get_claude_ai_oauth_tokens().map(|tokens| tokens.access_token);

        let _ = crate::utils::auth::check_and_refresh_oauth_token_if_needed(false).await?;
        let sent_token =
            read_access_token().ok_or_else(|| anyhow::anyhow!("No claude.ai OAuth token found"))?;
        match serve_claude_ai_proxy_http_with_token(name, proxy_url, sent_token.clone()).await {
            Ok(service) => Ok(service),
            Err(error) if is_auth_error(&error) => {
                let token_changed =
                    match crate::utils::auth::handle_oauth_401_error(&sent_token).await {
                        Ok(changed) => changed,
                        Err(gate_error)
                            if gate_error
                                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                                .is_some() =>
                        {
                            return Err(gate_error);
                        }
                        Err(_) => false,
                    };
                let current_token =
                    match crate::utils::auth::check_and_refresh_oauth_token_if_needed(false).await {
                        Ok(_) => read_access_token(),
                        Err(gate_error)
                            if gate_error
                                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                                .is_some() =>
                        {
                            return Err(gate_error);
                        }
                        Err(_) => None,
                    };
                if token_changed
                    || current_token
                        .as_deref()
                        .is_some_and(|now| now != sent_token)
                {
                    if let Some(current_token) = current_token {
                        if let Ok(service) =
                            serve_claude_ai_proxy_http_with_token(name, proxy_url, current_token)
                                .await
                        {
                            return Ok(service);
                        }
                    }
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    async fn serve_legacy_sse(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        headers.extend(
            crate::services::mcp::headers_helper::get_mcp_server_headers(name, config).await,
        );
        if let Some(token) = crate::services::mcp::auth::ClaudeAuthProvider::new(name, config)
            .tokens()?
            .and_then(|tokens| tokens.access_token)
            .filter(|token| !token.is_empty())
        {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        let transport = legacy_sse_transport(name, url, headers).await?;
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    async fn serve_legacy_sse_ide(
        name: &str,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        let transport = legacy_sse_transport(name, url, headers).await?;
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    async fn serve_websocket(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        let (session_bearer, _) = session_ingress_auth();
        if let Some(token) = session_bearer {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        headers.extend(
            crate::services::mcp::headers_helper::get_mcp_server_headers(name, config).await,
        );
        let transport = crate::utils::mcp_websocket_transport::connect_mcp_websocket_transport(
            name, url, headers,
        )
        .await?;
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    async fn serve_websocket_ide(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        if let Some(auth_token) = config
            .auth_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
        {
            headers.insert(
                "X-Claude-Code-Ide-Authorization".to_string(),
                auth_token.to_string(),
            );
        }
        let transport = crate::utils::mcp_websocket_transport::connect_mcp_websocket_transport(
            name, url, headers,
        )
        .await?;
        let handler = CometixMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    async fn serve_transport(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        match config.transport {
            Transport::Stdio => serve_stdio(name, config).await,
            Transport::Sse => {
                let url = config
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("SSE MCP server is missing URL"))?;
                serve_legacy_sse(name, config, url).await
            }
            Transport::SseIde => {
                let url = config
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("SSE IDE MCP server is missing URL"))?;
                serve_legacy_sse_ide(name, url).await
            }
            Transport::Http => {
                let url = config
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("HTTP MCP server is missing URL"))?;
                serve_streamable_http(name, config, url).await
            }
            Transport::Ws => {
                let url = config
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("WebSocket MCP server is missing URL"))?;
                serve_websocket(name, config, url).await
            }
            Transport::WsIde => {
                let url = config
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("WebSocket IDE MCP server is missing URL"))?;
                serve_websocket_ide(name, config, url).await
            }
            Transport::ClaudeAiProxy => {
                // Maps to: CC `services/mcp/client.ts:879-890` claude.ai-proxy
                // branch, consuming canonical `getOauthConfig().MCP_PROXY_*`.
                let id = config
                    .id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("claude.ai MCP proxy server is missing connector id")
                    })?;
                let oauth = crate::constants::oauth::get_oauth_config()?;
                let proxy_url = format!(
                    "{}{}",
                    oauth.mcp_proxy_url,
                    oauth.mcp_proxy_path.replace("{server_id}", id)
                );
                serve_claude_ai_proxy_http(name, &proxy_url).await
            }
            Transport::Sdk => Err(anyhow::anyhow!(
                "SDK MCP servers are owned by the SDK boundary"
            )),
        }
    }

    fn extract_www_auth_scope_param(header: &str) -> Option<String> {
        let lower = header.to_ascii_lowercase();
        let needle = "scope=";
        let pos = lower.find(needle)?;
        let start = pos + needle.len();
        let value = header.get(start..)?;
        if let Some(stripped) = value.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            let end = value
                .find(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
                .unwrap_or(value.len());
            (end > 0).then(|| value[..end].to_string())
        }
    }

    fn streamable_http_challenge_from_error(
        error: &StreamableHttpError<reqwest::Error>,
    ) -> Option<(String, Option<String>)> {
        // Maps to: CC `StreamableHTTPClientTransport` 401/403 handling plus
        // `wrapFetchWithStepUpDetection(...)` WWW-Authenticate extraction.
        match error {
            StreamableHttpError::AuthRequired(error) => {
                Some((error.www_authenticate_header.clone(), None))
            }
            StreamableHttpError::InsufficientScope(error) => Some((
                error.www_authenticate_header.clone(),
                error.required_scope.clone(),
            )),
            _ => None,
        }
    }

    fn streamable_http_challenge_from_dynamic_error(
        error: &DynamicTransportError,
    ) -> Option<(String, Option<String>)> {
        error
            .error
            .downcast_ref::<StreamableHttpError<reqwest::Error>>()
            .and_then(streamable_http_challenge_from_error)
    }

    pub(super) fn streamable_http_challenge_from_anyhow(
        error: &anyhow::Error,
    ) -> Option<(String, Option<String>)> {
        for cause in error.chain() {
            if let Some(error) = cause.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
                if let Some(challenge) = streamable_http_challenge_from_error(error) {
                    return Some(challenge);
                }
            }
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>() {
                if let Some(header) = error.www_authenticate.clone() {
                    return Some((header.clone(), extract_www_auth_scope_param(&header)));
                }
            }
            if let Some(ClientInitializeError::TransportError { error, .. }) =
                cause.downcast_ref::<ClientInitializeError>()
            {
                if let Some(challenge) = streamable_http_challenge_from_dynamic_error(error) {
                    return Some(challenge);
                }
            }
            if let Some(ServiceError::TransportSend(error)) = cause.downcast_ref::<ServiceError>() {
                if let Some(challenge) = streamable_http_challenge_from_dynamic_error(error) {
                    return Some(challenge);
                }
            }
        }
        None
    }

    pub(super) fn is_insufficient_scope_auth_error(error: &anyhow::Error) -> bool {
        for cause in error.chain() {
            if let Some(StreamableHttpError::InsufficientScope(_)) =
                cause.downcast_ref::<StreamableHttpError<reqwest::Error>>()
            {
                return true;
            }
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>() {
                if error.status == 403
                    && error
                        .www_authenticate
                        .as_deref()
                        .is_some_and(|header| header.contains("insufficient_scope"))
                {
                    return true;
                }
            }
            if let Some(ClientInitializeError::TransportError { error, .. }) =
                cause.downcast_ref::<ClientInitializeError>()
            {
                if let Some(StreamableHttpError::InsufficientScope(_)) =
                    error
                        .error
                        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
                {
                    return true;
                }
            }
            if let Some(ServiceError::TransportSend(error)) = cause.downcast_ref::<ServiceError>() {
                if let Some(StreamableHttpError::InsufficientScope(_)) =
                    error
                        .error
                        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
                {
                    return true;
                }
            }
        }
        false
    }

    fn record_mcp_auth_challenge_from_error(
        name: &str,
        config: &ScopedMcpServerConfig,
        transport_type: &str,
        url: &str,
        error: &anyhow::Error,
    ) -> anyhow::Result<()> {
        if let Some((www_authenticate_header, required_scope)) =
            streamable_http_challenge_from_anyhow(error)
        {
            if let Err(record_error) =
                crate::services::mcp::auth::record_mcp_www_authenticate_challenge(
                    name,
                    config,
                    transport_type,
                    url,
                    &www_authenticate_header,
                    required_scope.as_deref(),
                )
            {
                if record_error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some()
                {
                    return Err(record_error);
                }
                tracing::debug!(server = name, error = %record_error, "failed to persist MCP WWW-Authenticate challenge");
            }
        }
        Ok(())
    }

    fn remote_auth_context<'a>(
        config: &'a ScopedMcpServerConfig,
    ) -> Option<(&'static str, &'a str)> {
        match config.transport {
            Transport::Http => config.url.as_deref().map(|url| ("http", url)),
            Transport::Sse => config.url.as_deref().map(|url| ("sse", url)),
            _ => None,
        }
    }

    fn record_remote_auth_challenge(
        name: &str,
        config: &ScopedMcpServerConfig,
        error: &anyhow::Error,
    ) -> anyhow::Result<()> {
        if let Some((transport_type, url)) = remote_auth_context(config) {
            record_mcp_auth_challenge_from_error(name, config, transport_type, url, error)?;
        }
        Ok(())
    }

    async fn refresh_after_remote_auth_failure(
        name: &str,
        config: &ScopedMcpServerConfig,
        error: &anyhow::Error,
    ) -> anyhow::Result<bool> {
        // Maps to: CC MCP SDK 401 send/start retry. Do not refresh for
        // insufficient_scope step-up: CC marks step-up pending and falls
        // through to a new authorization URL instead of attempting an OAuth
        // refresh that cannot elevate scope (RFC 6749 §6).
        if is_insufficient_scope_auth_error(error) {
            return Ok(false);
        }
        let Some((transport_type, url)) = remote_auth_context(config) else {
            return Ok(false);
        };
        match crate::services::mcp::auth::refresh_mcp_oauth_access_token_after_auth_failure(
            name,
            config,
            transport_type,
            url,
        )
        .await
        {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(refresh_error)
                if refresh_error
                    .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>(
                    )
                    .is_some() =>
            {
                Err(refresh_error)
            }
            Err(refresh_error) => {
                tracing::debug!(server = name, error = %refresh_error, "MCP OAuth refresh-after-401 failed");
                Ok(false)
            }
        }
    }

    async fn serve_transport_with_auth_retry(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<RunningService<RoleClient, CometixMcpClientHandler>> {
        match serve_transport(name, config).await {
            Ok(service) => Ok(service),
            Err(error) if is_auth_error(&error) => {
                record_remote_auth_challenge(name, config, &error)?;
                if refresh_after_remote_auth_failure(name, config, &error).await? {
                    match serve_transport(name, config).await {
                        Ok(service) => return Ok(service),
                        Err(retry_error) => {
                            if is_auth_error(&retry_error) {
                                record_remote_auth_challenge(name, config, &retry_error)?;
                            }
                            Err(retry_error)
                        }
                    }
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn is_auth_error(error: &anyhow::Error) -> bool {
        for cause in error.chain() {
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>() {
                if matches!(error.status, 401 | 403) {
                    return true;
                }
            }
            if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
                if error
                    .status()
                    .is_some_and(|status| matches!(status.as_u16(), 401 | 403))
                {
                    return true;
                }
            }
            if matches!(
                cause.downcast_ref::<StreamableHttpError<reqwest::Error>>(),
                Some(StreamableHttpError::AuthRequired(_))
                    | Some(StreamableHttpError::InsufficientScope(_))
            ) {
                return true;
            }
        }
        let message = error.to_string().to_ascii_lowercase();
        message.contains("401")
            || message.contains("unauthorized")
            || message.contains("authrequired")
            || message.contains("auth required")
            || message.contains("insufficient scope")
    }

    fn is_session_expired_error(error: &anyhow::Error) -> bool {
        // Maps to: CC `isMcpSessionExpiredError(...)` and the follow-up
        // "Connection closed" check for streamable HTTP transports.
        let message = error.to_string().to_ascii_lowercase();
        (message.contains("404")
            && (message.contains("-32001") || message.contains("session not found")))
            || (message.contains("-32000") && message.contains("connection closed"))
    }

    fn mcp_error_from_anyhow(error: &anyhow::Error) -> Option<&McpError> {
        error.downcast_ref::<McpError>().or_else(|| {
            error
                .downcast_ref::<ServiceError>()
                .and_then(|service_error| {
                    if let ServiceError::McpError(error) = service_error {
                        Some(error)
                    } else {
                        None
                    }
                })
        })
    }

    pub(super) fn is_url_elicitation_required_error(error: &anyhow::Error) -> bool {
        // Maps to: CC `error.code !== ErrorCode.UrlElicitationRequired`
        // check in `callMCPToolWithUrlElicitationRetry(...)`.
        mcp_error_from_anyhow(error)
            .is_some_and(|error| error.code == ErrorCode::URL_ELICITATION_REQUIRED)
            || error.to_string().contains("-32042")
    }

    pub(super) fn url_elicitations_from_error(
        error: &anyhow::Error,
    ) -> Vec<RmcpElicitRequestParams> {
        // Maps to: CC validation of `error.data.elicitations` elements in
        // `callMCPToolWithUrlElicitationRetry(...)`.
        let Some(error) = mcp_error_from_anyhow(error) else {
            return Vec::new();
        };
        let Some(elicitations) = error
            .data
            .as_ref()
            .and_then(|data| data.get("elicitations"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        elicitations
            .iter()
            .filter_map(|entry| {
                let object = entry.as_object()?;
                if object.get("mode").and_then(Value::as_str) != Some("url") {
                    return None;
                }
                let url = object.get("url").and_then(Value::as_str)?.to_string();
                let elicitation_id = object
                    .get("elicitationId")
                    .and_then(Value::as_str)?
                    .to_string();
                let message = object.get("message").and_then(Value::as_str)?.to_string();
                Some(RmcpElicitRequestParams::UrlElicitationParams {
                    meta: None,
                    message,
                    url,
                    elicitation_id,
                })
            })
            .collect()
    }

    fn mcp_tool_text_result(message: String) -> Value {
        serde_json::json!({
            "content": [{ "type": "text", "text": message }]
        })
    }

    fn tool_snapshot(tool: rmcp::model::Tool) -> McpToolSnapshot {
        let annotations = tool.annotations.clone().unwrap_or_default();
        McpToolSnapshot {
            name: tool.name.to_string(),
            display_name: annotations.title.or(tool.title),
            description: tool.description.map(|value| value.to_string()),
            input_schema: Value::Object((*tool.input_schema).clone()),
            read_only_hint: annotations.read_only_hint.unwrap_or(false),
            destructive_hint: annotations.destructive_hint.unwrap_or(false),
            open_world_hint: annotations.open_world_hint.unwrap_or(false),
        }
    }

    fn prompt_snapshot(prompt: rmcp::model::Prompt) -> McpPromptSnapshot {
        McpPromptSnapshot {
            name: prompt.name,
            description: prompt.description,
            arg_names: prompt
                .arguments
                .unwrap_or_default()
                .into_iter()
                .map(|argument| argument.name)
                .collect(),
        }
    }

    /// Maps to: CC `client.ts:2017-2020` — the map inside
    /// `fetchResourcesForClient` that stamps the owning server onto every
    /// entry the moment it leaves the SDK:
    /// `result.resources.map(resource => ({ ...resource, server: client.name }))`.
    ///
    /// `rmcp::model::Resource` is the SDK shape (CC's `Resource`), which has no
    /// `server`; everything downstream of this function is `ServerResource`,
    /// exactly as in CC where `fetchResourcesForClient` is typed
    /// `Promise<ServerResource[]>` and `AppState.mcp.resources` is
    /// `Record<string, ServerResource[]>`.
    fn resource_snapshot(resource: rmcp::model::Resource, server: &str) -> ServerResource {
        ServerResource {
            server: server.to_string(),
            uri: resource.uri,
            name: resource.name,
            description: resource.description,
            mime_type: resource.mime_type,
        }
    }

    async fn fetch_tools_for_peer(peer: &Peer<RoleClient>) -> Vec<McpToolSnapshot> {
        let Some(peer_info) = peer.peer_info() else {
            return Vec::new();
        };
        if peer_info.capabilities.tools.is_none() {
            return Vec::new();
        }
        match peer.list_all_tools().await {
            Ok(tools) => tools.into_iter().map(tool_snapshot).collect(),
            Err(error) => {
                tracing::warn!(error = %error, "failed to fetch MCP tools");
                Vec::new()
            }
        }
    }

    /// Maps to: CC `services/mcp/client.ts:1726` `MCP_FETCH_CACHE_SIZE`.
    const MCP_FETCH_CACHE_SIZE: usize = 20;

    /// Maps to: CC `client.ts:2000-2031`
    /// `fetchResourcesForClient = memoizeWithLRU(..., client.name, MCP_FETCH_CACHE_SIZE)`.
    /// Insertion order is the recency order; the front entry is the LRU victim.
    static RESOURCES_FETCH_CACHE: std::sync::LazyLock<
        tokio::sync::Mutex<indexmap::IndexMap<String, Vec<ServerResource>>>,
    > = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(indexmap::IndexMap::new()));

    /// Maps to: CC `fetchResourcesForClient.cache.delete(name)`
    /// (`client.ts:1390` onclose, `:1668` reconnect, `resources/list_changed`).
    pub async fn delete_resources_fetch_cache_entry(name: &str) {
        RESOURCES_FETCH_CACHE.lock().await.shift_remove(name);
    }

    /// LRU-memoized counterpart of [`fetch_resources_for_peer`]. Every caller
    /// that CC routes through `fetchResourcesForClient` must use this.
    pub(super) async fn fetch_resources_for_client(
        peer: &Peer<RoleClient>,
        server: &str,
    ) -> Vec<ServerResource> {
        {
            let mut cache = RESOURCES_FETCH_CACHE.lock().await;
            if let Some(cached) = cache.shift_remove(server) {
                cache.insert(server.to_string(), cached.clone());
                return cached;
            }
        }
        let resources = fetch_resources_for_peer(peer, server).await;
        let mut cache = RESOURCES_FETCH_CACHE.lock().await;
        cache.shift_remove(server);
        cache.insert(server.to_string(), resources.clone());
        while cache.len() > MCP_FETCH_CACHE_SIZE {
            cache.shift_remove_index(0);
        }
        resources
    }

    async fn fetch_resources_for_peer(
        peer: &Peer<RoleClient>,
        server: &str,
    ) -> Vec<ServerResource> {
        let Some(peer_info) = peer.peer_info() else {
            return Vec::new();
        };
        if peer_info.capabilities.resources.is_none() {
            return Vec::new();
        }
        match peer.list_all_resources().await {
            Ok(resources) => resources
                .into_iter()
                .map(|resource| resource_snapshot(resource, server))
                .collect(),
            Err(error) => {
                tracing::warn!(error = %error, "failed to fetch MCP resources");
                Vec::new()
            }
        }
    }

    async fn fetch_commands_for_peer(peer: &Peer<RoleClient>) -> Vec<McpPromptSnapshot> {
        let Some(peer_info) = peer.peer_info() else {
            return Vec::new();
        };
        if peer_info.capabilities.prompts.is_none() {
            return Vec::new();
        }
        match peer.list_all_prompts().await {
            Ok(prompts) => prompts.into_iter().map(prompt_snapshot).collect(),
            Err(error) => {
                tracing::warn!(error = %error, "failed to fetch MCP prompts");
                Vec::new()
            }
        }
    }

    async fn connected_peer(name: &str) -> anyhow::Result<Peer<RoleClient>> {
        CONNECTED_CLIENTS
            .lock()
            .unwrap()
            .get(name)
            .map(|client| client.peer.clone())
            .ok_or_else(|| anyhow::anyhow!("MCP server \"{name}\" is not connected"))
    }

    /// Maps to: CC `fetchToolsForClient.cache.delete(name); fetchToolsForClient(client)`
    /// in the `tools/list_changed` notification handler.
    pub async fn refresh_mcp_tools_for_client(name: &str) -> anyhow::Result<Vec<McpToolSnapshot>> {
        let peer = connected_peer(name).await?;
        Ok(fetch_tools_for_peer(&peer).await)
    }

    /// Maps to: CC `fetchCommandsForClient.cache.delete(name); fetchCommandsForClient(client)`
    /// in the `prompts/list_changed` notification handler.
    pub async fn refresh_mcp_prompts_for_client(
        name: &str,
    ) -> anyhow::Result<Vec<McpPromptSnapshot>> {
        let peer = connected_peer(name).await?;
        Ok(fetch_commands_for_peer(&peer).await)
    }

    /// Maps to: CC `fetchResourcesForClient.cache.delete(name); fetchResourcesForClient(client)`
    /// in the `resources/list_changed` notification handler.
    /// Server name -> its declared `capabilities.experimental`, read off the
    /// live peers.
    ///
    /// CC never needs this: its `AppState.mcp.clients` entries ARE the
    /// connection records, so `connection.capabilities.experimental` is in hand
    /// wherever the clients are (`cli/print.ts:1673`,
    /// `channelPermissions.ts:191-192`). Rust keeps capabilities on the peer
    /// rather than the snapshot (see `McpServerSnapshot`'s doc), so consumers
    /// holding only a snapshot have to ask the registry for them.
    ///
    /// rmcp types each experimental entry as a JSON object; CC only tests key
    /// presence, so values are carried through unchanged.
    pub async fn experimental_capabilities_by_server()
    -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>> {
        let clients = CONNECTED_CLIENTS.lock().unwrap();
        clients
            .iter()
            .map(|(name, client)| {
                let experimental = client
                    .peer
                    .peer_info()
                    .and_then(|info| info.capabilities.experimental.clone())
                    .map(|map| {
                        map.into_iter()
                            .map(|(key, value)| (key, Value::Object(value)))
                            .collect()
                    })
                    .unwrap_or_default();
                (name.clone(), experimental)
            })
            .collect()
    }

    pub async fn refresh_mcp_resources_for_client(
        name: &str,
    ) -> anyhow::Result<Vec<ServerResource>> {
        let peer = connected_peer(name).await?;
        delete_resources_fetch_cache_entry(name).await;
        Ok(fetch_resources_for_client(&peer, name).await)
    }

    /// Maps to: CC `ListMcpResourcesTool`'s `fetchResourcesForClient(fresh)`
    /// (`ListMcpResourcesTool.ts:89`) — LRU-cached, never refreshed by the tool.
    pub async fn fetch_mcp_resources_for_client(name: &str) -> anyhow::Result<Vec<ServerResource>> {
        let peer = connected_peer(name).await?;
        Ok(fetch_resources_for_client(&peer, name).await)
    }

    pub async fn drain_mcp_connection_callback_observations()
    -> Vec<crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation> {
        crate::services::mcp::use_manage_mcp_connections::drain_mcp_connection_callback_observations().await
    }

    pub async fn respond_to_mcp_elicitation(
        server_name: &str,
        request_id: &str,
        result: ElicitationResult,
    ) -> bool {
        // Maps to: CC `ElicitationRequestEvent.respond` resolving the pending
        // `ElicitResult` promise inside `registerElicitationHandler(...)`.
        let sender = PENDING_ELICITATION_RESPONSES
            .lock()
            .await
            .remove(&(server_name.to_string(), request_id.to_string()));
        sender.is_some_and(|sender| sender.send(result).is_ok())
    }

    async fn wait_for_url_elicitation_retry(
        server_name: &str,
        request: &RmcpElicitRequestParams,
    ) -> ElicitationResult {
        // Maps to: CC REPL queue branch in
        // `callMCPToolWithUrlElicitationRetry(...)` for -32042 errors.
        let elicitation_id = match request {
            RmcpElicitRequestParams::UrlElicitationParams { elicitation_id, .. } => {
                elicitation_id.clone()
            }
            _ => return ElicitationResult::new(ElicitationAction::Cancel),
        };
        let request_id = format!("error-elicit-{elicitation_id}");
        let (sender, receiver) = oneshot::channel::<ElicitationResult>();
        PENDING_ELICITATION_RESPONSES
            .lock()
            .await
            .insert((server_name.to_string(), request_id.clone()), sender);
        let event = ElicitationRequestEvent::new(
            server_name.to_string(),
            request_id,
            rmcp_elicitation_params_to_event_params(request),
        )
        .with_error_retry_waiting_state();
        crate::services::mcp::use_manage_mcp_connections::emit_elicitation_requested(event).await;
        receiver
            .await
            .unwrap_or_else(|_| ElicitationResult::new(ElicitationAction::Cancel))
    }

    pub(super) async fn process_url_elicitation_required(
        server_name: &str,
        tool: &str,
        elicitations: Vec<RmcpElicitRequestParams>,
        handle_elicitation: crate::tool::HandleElicitationCallback,
    ) -> Option<Value> {
        for elicitation in elicitations {
            if let Some(hook_response) =
                run_elicitation_hooks_for_request(server_name, &elicitation).await
            {
                if hook_response.action != ElicitationAction::Accept {
                    return Some(mcp_tool_text_result(
                        url_elicitation_required_result_message(
                            hook_response.action,
                            "a hook",
                            tool,
                        ),
                    ));
                }
                continue;
            }

            // Resolve via callback (print/SDK mode) or queue (REPL mode) —
            // CC client.ts:2944-2947.
            let user_result = if let Some(handler) = handle_elicitation.get() {
                handler(
                    server_name.to_string(),
                    rmcp_elicitation_params_to_event_params(&elicitation),
                )
                .await
            } else {
                wait_for_url_elicitation_retry(server_name, &elicitation).await
            };
            let final_result =
                run_elicitation_result_hooks_for_response(server_name, &elicitation, user_result)
                    .await;
            if final_result.action != ElicitationAction::Accept {
                return Some(mcp_tool_text_result(
                    url_elicitation_required_result_message(final_result.action, "the user", tool),
                ));
            }
        }
        None
    }

    /// Maps to: CC useIdeSelection.ts:73-83 — the ide client leaving (or being
    /// replaced in) the client set is an identity change, so the REPL selection
    /// resets to the CC-shaped empty OBJECT before this connection's sink
    /// clone drops with its registry entry.
    pub(super) fn notify_ide_selection_identity_changed(
        sink: &async_channel::Sender<IdeSelection>,
    ) {
        let _ = sink.try_send(ide_selection_reset());
    }

    fn spawn_service_close_watcher(
        name: String,
        config: ScopedMcpServerConfig,
        connection_id: u64,
        service: RunningService<RoleClient, CometixMcpClientHandler>,
    ) {
        tokio::spawn(async move {
            let reason = service.waiting().await;
            let removed = {
                let mut clients = CONNECTED_CLIENTS.lock().unwrap();
                if clients
                    .get(&name)
                    .is_some_and(|client| client.connection_id == connection_id)
                {
                    clients.remove(&name)
                } else {
                    None
                }
            };
            let Some(removed) = removed else {
                return;
            };
            // Identity change ide → null: reset, then drop the per-connection
            // sink with the entry (CC ":148 no cleanup needed").
            if let Some(sink) = removed.ide_selection_sink.as_ref() {
                notify_ide_selection_identity_changed(sink);
            }
            clear_mcp_server_instructions(&name);
            if !removed.on_close_enabled {
                return;
            }
            match reason {
                Ok(QuitReason::Cancelled) => {}
                Ok(_) | Err(_) => {
                    // Maps to: CC `client.client.onclose = () => { ... }` in
                    // `useManageMCPConnections.ts#onConnectionAttempt`.
                    crate::services::mcp::use_manage_mcp_connections::emit_server_closed(
                        name, config,
                    )
                    .await;
                }
            }
        });
    }

    async fn replace_cached_client(
        name: String,
        config: ScopedMcpServerConfig,
        service: RunningService<RoleClient, CometixMcpClientHandler>,
    ) -> u64 {
        let previous = {
            let mut clients = CONNECTED_CLIENTS.lock().unwrap();
            clients.remove(&name)
        };
        if let Some(previous) = previous {
            // No reset send here: replacing old → new is ONE identity change
            // in CC (useIdeSelection.ts:73-83); the insertion below sends it.
            if let Some(token) = previous.cancellation_token.lock().unwrap().take() {
                token.cancel();
            }
        }

        let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
        // Maps to: CC `useIdeSelection` registering its `selection_changed`
        // handler on the (new) ide client: capture the REPL sink per
        // connection, keyed by this connection's identity.
        let ide_selection_sink = ide_selection_sink_for_new_connection(&name);
        let _ = service.service().connection_id.set(connection_id);
        let peer = service.peer().clone();
        let cancellation_token =
            Arc::new(std::sync::Mutex::new(Some(service.cancellation_token())));
        let mut clients = CONNECTED_CLIENTS.lock().unwrap();
        // Identity change (null|old) → new ide client: send the reset BEFORE
        // attaching the sink to the entry, so no selection notification can
        // land in between and be clobbered by the reset (CC orders the reset
        // at useIdeSelection.ts:77 ahead of handler registration at :112).
        if let Some(sink) = ide_selection_sink.as_ref() {
            notify_ide_selection_identity_changed(sink);
        }
        clients.insert(
            name.clone(),
            ConnectedMcpClient {
                on_close_enabled: true,
                config: config.clone(),
                peer,
                cancellation_token,
                connection_id,
                ide_selection_sink: ide_selection_sink.clone(),
            },
        );
        drop(clients);
        spawn_service_close_watcher(name, config, connection_id, service);
        connection_id
    }

    /// The REPL selection sender a newly cached connection captures — `Some`
    /// only for the strict CC ide client name gate (utils/ide.ts:1251).
    pub(super) fn ide_selection_sink_for_new_connection(
        name: &str,
    ) -> Option<async_channel::Sender<IdeSelection>> {
        if !is_ide_mcp_server_name(name) {
            return None;
        }
        crate::services::mcp::use_manage_mcp_connections::current_ide_selection_sink()
    }

    /// Maps to: CC `useIdeSelection(mcp.clients, setIDESelection)`
    /// (useIdeSelection.ts:59-150). The REPL registers its selection sender
    /// once; the registry clones it into the live "ide" connection (if any)
    /// and into every future one via `replace_cached_client`. Registering onto
    /// an already-connected ide client is the CC first-effect identity change
    /// (null → client, useIdeSelection.ts:73-83) and therefore resets.
    pub fn register_ide_selection_sink(sink: async_channel::Sender<IdeSelection>) {
        crate::services::mcp::use_manage_mcp_connections::set_ide_selection_sink(sink.clone());
        // A3 audit (PORTING.md § "Node-async → tokio"): the caller is the REPL render
        // side (repl.rs), where the ambient runtime IS the process runtime, so
        // the old `try_current()` was accidentally correct. Use the explicit
        // detach handle anyway per the A3 rule — the attach must survive
        // whatever context a future caller runs in. Outside any runtime
        // (component unit tests) there are no cached connections to attach to.
        let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
            return;
        };
        handle.spawn(async move {
            let mut clients = CONNECTED_CLIENTS.lock().unwrap();
            if let Some((_, entry)) = clients
                .iter_mut()
                .find(|(name, _)| is_ide_mcp_server_name(name))
            {
                entry.ide_selection_sink = Some(sink.clone());
                drop(clients);
                notify_ide_selection_identity_changed(&sink);
            }
        });
    }

    /// Native mutation of the live connection's optional onclose callback.
    /// Maps to: CC `useManageMCPConnections.ts:808` `s.client.onclose = undefined`.
    pub fn detach_mcp_close_handler(name: &str, connection_id: Option<u64>) {
        if let Some(client) = CONNECTED_CLIENTS.lock().unwrap().get_mut(name) {
            if Some(client.connection_id) == connection_id {
                client.on_close_enabled = false;
            }
        }
    }

    /// Capture the live instance at invocation, before the returned future is
    /// polled. This represents CC clearServerCache's pre-await memo lookup.
    pub fn clear_server_cache(
        name: &str,
        server_ref: Option<&ScopedMcpServerConfig>,
    ) -> impl Future<Output = ()> + Send + 'static {
        let name = name.to_owned();
        let captured = CONNECTED_CLIENTS
            .lock()
            .unwrap()
            .get(&name)
            .filter(|client| server_ref.is_none_or(|config| config == &client.config))
            .cloned();
        async move {
            if let Some(previous) = captured {
                if let Some(sink) = previous.ide_selection_sink.as_ref() {
                    notify_ide_selection_identity_changed(sink);
                }
                if let Some(token) = previous.cancellation_token.lock().unwrap().take() {
                    token.cancel();
                }
                let mut clients = CONNECTED_CLIENTS.lock().unwrap();
                if clients
                    .get(&name)
                    .is_some_and(|client| client.connection_id == previous.connection_id)
                {
                    clients.remove(&name);
                }
            }
            clear_mcp_server_instructions(&name);
            // Fetch caches remain name-keyed even if a newer live instance exists.
            delete_resources_fetch_cache_entry(&name).await;
        }
    }

    pub async fn is_connected_mcp_client(name: &str) -> bool {
        // Maps to: CC `vscodeSdkMcp.ts` checking the stored connected client.
        CONNECTED_CLIENTS.lock().unwrap().contains_key(name)
    }

    pub async fn send_custom_notification_to_connected_client(
        name: &str,
        method: &str,
        params: Value,
    ) -> anyhow::Result<()> {
        // Maps to: CC `ConnectedMCPServer.client.notification({ method, params })`.
        let peer = CONNECTED_CLIENTS
            .lock()
            .unwrap()
            .get(name)
            .map(|client| client.peer.clone())
            .ok_or_else(|| anyhow::anyhow!("MCP server \"{name}\" is not connected"))?;
        let notification = CustomNotification::new(method.to_string(), Some(params));
        peer.send_notification(ClientNotification::CustomNotification(notification))
            .await
            .map_err(anyhow::Error::new)
    }

    pub async fn connect_to_server(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        let service = match serve_transport_with_auth_retry(name, config).await {
            Ok(service) => service,
            Err(error)
                if remote_transport_uses_auth_cache(config.transport) && is_auth_error(&error) =>
            {
                if let Err(gate_error) = record_remote_auth_challenge(name, config, &error) {
                    return McpConnectionDiscovery::failed(name, config, gate_error.to_string());
                }
                set_mcp_auth_cache_entry(name);
                return McpConnectionDiscovery::needs_auth(name, config);
            }
            Err(error) if is_auth_error(&error) => {
                if let Err(gate_error) = record_remote_auth_challenge(name, config, &error) {
                    return McpConnectionDiscovery::failed(name, config, gate_error.to_string());
                }
                return McpConnectionDiscovery::needs_auth(name, config);
            }
            Err(error) => {
                return {
                    crate::utils::debug::log_for_debugging(&format!(
                        "[MCP] {name} connect error: {error}"
                    ));
                    McpConnectionDiscovery::failed(name, config, error.to_string())
                };
            }
        };

        let peer = service.peer().clone();
        let server_version = peer
            .peer_info()
            .map(|info| info.server_info.version.clone());
        let instructions = peer.peer_info().and_then(|info| info.instructions.clone());
        let supports_resources = peer
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some());
        let tools = fetch_tools_for_peer(&peer).await;
        let prompts = fetch_commands_for_peer(&peer).await;
        let resources = fetch_resources_for_client(&peer, name).await;

        set_mcp_server_instructions(name, instructions);
        let connection_id = replace_cached_client(name.to_string(), config.clone(), service).await;

        let subscribe = supports_resources;
        crate::utils::debug::log_for_debugging(&format!(
            "[MCP] Server \"{name}\" connected with subscribe={subscribe}"
        ));

        McpConnectionDiscovery::from(McpServerSnapshot {
            connection_id: Some(connection_id),
            client: McpClientSnapshot {
                name: name.to_string(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: config
                    .ide_name
                    .clone()
                    .or_else(|| (name == "ide").then(|| "IDE".to_string())),
                server_version,
                error: None,
            },
            config: Some(config.clone()),
            supports_resources,
            tools,
            prompts,
            resources,
        })
    }

    pub async fn setup_sdk_mcp_clients(
        sdk_mcp_configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
        send_mcp_message: SendMcpMessageCallback,
    ) -> SdkMcpClientsSetup {
        // Maps to: CC `services/mcp/client.ts#setupSdkMcpClients`: create an
        // SDK control transport per configured server, connect an MCP client,
        // fetch tools if the server advertises them, and return failed rows for
        // per-server connection failures.
        let clients = futures::future::join_all(crate::utils::process_env::ecmascript_object_entries(sdk_mcp_configs).into_iter().map(|(name, config)| {
            let name = name.to_owned();
            let config = config.clone();
            let send_mcp_message = send_mcp_message.clone();
            async move {
                clear_mcp_server_instructions(&name);
                let mut connected_config = config.clone();
                connected_config.scope = crate::services::mcp::types::ConfigScope::Dynamic;
                let transport = SdkControlClientTransport::new(name.clone(), send_mcp_message);
                let handler = CometixMcpClientHandler::new(&name);
                match serve_client(handler, transport).await {
                    Ok(service) => {
                        let peer = service.peer().clone();
                        let server_version = peer
                            .peer_info()
                            .map(|info| info.server_info.version.clone());
                        let instructions = peer
                            .peer_info()
                            .and_then(|info| info.instructions.clone());
                        let supports_resources = peer
                            .peer_info()
                            .is_some_and(|info| info.capabilities.resources.is_some());
                        let tools = if peer.peer_info().is_some_and(|info| info.capabilities.tools.is_some()) {
                            fetch_tools_for_peer(&peer).await
                        } else { Vec::new() };
                        let prompts = Vec::new();
                        let resources = Vec::new();
                        set_mcp_server_instructions(&name, instructions);
                        let connection_id = replace_cached_client(name.clone(), connected_config.clone(), service)
                            .await;
                        McpServerSnapshot {
                            connection_id: Some(connection_id),
                            client: McpClientSnapshot {
                                name,
                                status: McpServerConnectionType::Connected,
                                reconnect_attempt: None,
                                max_reconnect_attempts: None,
                                ide_name: connected_config.ide_name.clone(),
                                server_version,
                                error: None,
                            },
                            config: Some(connected_config),
                            supports_resources,
                            tools,
                            prompts,
                            resources,
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, server = %name, "failed to connect SDK MCP server");
                        let mut failed_config = config.clone();
                        failed_config.scope = crate::services::mcp::types::ConfigScope::User;
                        McpConnectionDiscovery::failed(name, &failed_config, error.to_string())
                            .server
                    }
                }
            }
        })).await;
        let tools = project_mcp_server_tools(&clients);
        SdkMcpClientsSetup { clients, tools }
    }

    pub async fn reconnect_mcp_server_impl(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        // Maps to CC `reconnectMcpServerImpl`: invalidate cache, reconnect,
        // then refresh tools/prompts/resources.
        clear_server_cache(name, None).await;
        let mut result = connect_to_server(name, config).await;
        result.add_reconnect_resource_tools();
        result
    }

    async fn discover_mcp_connection_entry(
        name: String,
        config: ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        let missing_oauth_token =
            crate::services::mcp::auth::has_mcp_discovery_but_no_token(&name, &config);
        if (remote_transport_uses_auth_cache(config.transport) && is_mcp_auth_cached(&name))
            || missing_oauth_token
        {
            McpConnectionDiscovery::needs_auth(name, &config)
        } else {
            connect_to_server(&name, &config).await
        }
    }

    /// Maps to: CC `client.ts#getMcpToolsCommandsAndResources`.
    pub async fn get_mcp_tools_commands_and_resources(
        on_connection_attempt: impl Fn(McpConnectionDiscovery) + Sync,
        configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    ) {
        let resource_tools_added = std::sync::atomic::AtomicBool::new(false);
        let mut local_servers = Vec::new();
        let mut remote_servers = Vec::new();
        for (name, config) in crate::utils::process_env::ecmascript_object_entries(configs) {
            // Source publishes disabled rows before either connection pool starts.
            if super::super::config::is_mcp_server_disabled(name) {
                on_connection_attempt(McpConnectionDiscovery::disabled(name, config));
            } else if is_local_mcp_server(config) {
                local_servers.push((name.to_owned(), config.clone()));
            } else {
                remote_servers.push((name.to_owned(), config.clone()));
            }
        }
        let process_server = |(name, config): (String, ScopedMcpServerConfig)| {
            let on_connection_attempt = &on_connection_attempt;
            let resource_tools_added = &resource_tools_added;
            async move {
                // The source repeats the disabled check when a queued slot starts.
                let mut discovery = if super::super::config::is_mcp_server_disabled(&name) {
                    McpConnectionDiscovery::disabled(name, &config)
                } else {
                    discover_mcp_connection_entry(name, config).await
                };
                discovery.add_discovery_resource_tools(resource_tools_added);
                on_connection_attempt(discovery);
            }
        };
        tokio::join!(
            process_batched(
                local_servers,
                get_mcp_server_connection_batch_size(),
                &process_server
            ),
            process_batched(
                remote_servers,
                get_remote_mcp_server_connection_batch_size(),
                &process_server
            ),
        );
    }

    /// Maps to CC `callMCPTool(...)` request payload `{ name, arguments,
    /// _meta }`, including the `claudecode/toolUseId` metadata forwarded by
    /// dynamic MCP tool calls.
    pub(super) fn call_tool_request_params(
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
    ) -> CallToolRequestParams {
        let mut params = CallToolRequestParams::new(tool_name.to_string()).with_arguments(args);
        params.meta = meta.map(Meta);
        params
    }

    async fn call_mcp_tool_once(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
    ) -> Result<Value, (anyhow::Error, Option<ScopedMcpServerConfig>)> {
        let timeout = Duration::from_millis(super::get_mcp_tool_timeout_ms_from_env(|key| {
            crate::utils::process_env::var(key)
        }));
        let (peer, config) = {
            let clients = CONNECTED_CLIENTS.lock().unwrap();
            let Some(client) = clients.get(server_name) else {
                return Err((
                    anyhow::anyhow!("MCP server \"{server_name}\" is not connected"),
                    None,
                ));
            };
            (client.peer.clone(), client.config.clone())
        };
        let params = call_tool_request_params(tool_name, args, meta);
        let call_result = tokio::time::timeout(timeout, peer.call_tool(params)).await;
        let result = match call_result {
            Err(_) => return Err((anyhow::anyhow!("MCP tool call timed out"), Some(config))),
            Ok(Ok(result)) => result,
            Ok(Err(error)) => return Err((anyhow::Error::new(error), Some(config))),
        };
        serde_json::to_value(result).map_err(|error| (anyhow::Error::new(error), None))
    }

    async fn handle_remote_call_auth_failure_and_retry_connect(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        error: &anyhow::Error,
    ) -> anyhow::Result<bool> {
        record_remote_auth_challenge(server_name, config, error)?;
        if refresh_after_remote_auth_failure(server_name, config, error).await? {
            clear_server_cache(server_name, None).await;
            let discovery = connect_to_server(server_name, config).await;
            return Ok(discovery.server.client.status == McpServerConnectionType::Connected);
        }
        set_mcp_auth_cache_entry(server_name);
        Ok(false)
    }

    fn record_final_remote_call_error(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        error: &anyhow::Error,
    ) -> anyhow::Result<()> {
        if is_auth_error(error) {
            record_remote_auth_challenge(server_name, config, error)?;
            set_mcp_auth_cache_entry(server_name);
        }
        Ok(())
    }

    pub async fn call_mcp_tool(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
    ) -> anyhow::Result<Value> {
        call_mcp_tool_with_meta(server_name, tool_name, args, None).await
    }

    pub async fn call_mcp_tool_with_meta(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
    ) -> anyhow::Result<Value> {
        call_mcp_tool_with_elicitation(
            server_name,
            tool_name,
            args,
            meta,
            crate::tool::HandleElicitationCallback::default(),
        )
        .await
    }

    pub async fn call_mcp_tool_with_elicitation(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
        handle_elicitation: crate::tool::HandleElicitationCallback,
    ) -> anyhow::Result<Value> {
        // Maps to: CC `callMCPToolWithUrlElicitationRetry(...)` wrapping the
        // low-level `callMCPTool(...)` call; `handle_elicitation` is the
        // print/SDK leg it receives from `context.handleElicitation`
        // (client.ts:1880).
        let mut url_elicitation_attempt = 0usize;
        loop {
            match call_mcp_tool_once(server_name, tool_name, args.clone(), meta.clone()).await {
                Ok(value) => return Ok(value),
                Err((error, Some(config))) => {
                    if is_url_elicitation_required_error(&error) {
                        if url_elicitation_attempt >= MAX_URL_ELICITATION_RETRIES {
                            return Err(error);
                        }
                        let elicitations = url_elicitations_from_error(&error);
                        if !elicitations.is_empty() {
                            if let Some(result) = process_url_elicitation_required(
                                server_name,
                                tool_name,
                                elicitations,
                                handle_elicitation.clone(),
                            )
                            .await
                            {
                                return Ok(result);
                            }
                            url_elicitation_attempt += 1;
                            continue;
                        }
                    }

                    if is_auth_error(&error)
                        && handle_remote_call_auth_failure_and_retry_connect(
                            server_name,
                            &config,
                            &error,
                        )
                        .await?
                    {
                        match call_mcp_tool_once(server_name, tool_name, args.clone(), meta.clone())
                            .await
                        {
                            Ok(value) => return Ok(value),
                            Err((retry_error, Some(retry_config))) => {
                                record_final_remote_call_error(
                                    server_name,
                                    &retry_config,
                                    &retry_error,
                                )?;
                                if is_session_expired_error(&retry_error) {
                                    clear_server_cache(server_name, None).await;
                                }
                                return Err(retry_error);
                            }
                            Err((retry_error, None)) => return Err(retry_error),
                        }
                    }
                    if is_session_expired_error(&error) {
                        clear_server_cache(server_name, None).await;
                    }
                    return Err(error);
                }
                Err((error, None)) => return Err(error),
            }
        }
    }

    async fn get_mcp_prompt_once(
        server_name: &str,
        prompt_name: &str,
        arg_names: &[String],
        args: &str,
    ) -> Result<Vec<Value>, (anyhow::Error, Option<ScopedMcpServerConfig>)> {
        let (peer, config) = {
            let clients = CONNECTED_CLIENTS.lock().unwrap();
            let Some(client) = clients.get(server_name) else {
                return Err((
                    anyhow::anyhow!("MCP server \"{server_name}\" is not connected"),
                    None,
                ));
            };
            (client.peer.clone(), client.config.clone())
        };
        let params = GetPromptRequestParams::new(prompt_name.to_string())
            .with_arguments(super::mcp_prompt_arguments_from_args(arg_names, args));
        let result = peer
            .get_prompt(params)
            .await
            .map_err(|error| (anyhow::Error::new(error), Some(config)))?;
        let mut blocks = Vec::new();
        for message in result.messages {
            let content = serde_json::to_value(message.content)
                .map_err(|error| (anyhow::Error::new(error), None))?;
            blocks.extend(
                super::transform_result_content(&content, server_name)
                    .map_err(|error| (error, None))?,
            );
        }
        Ok(blocks)
    }

    /// Maps to: CC MCP prompt command `getPromptForCommand(args)` from
    /// `fetchCommandsForClient(...)`.
    pub async fn get_mcp_prompt_for_command(
        server_name: &str,
        prompt_name: &str,
        arg_names: &[String],
        args: &str,
    ) -> anyhow::Result<Vec<Value>> {
        match get_mcp_prompt_once(server_name, prompt_name, arg_names, args).await {
            Ok(value) => Ok(value),
            Err((error, Some(config))) => {
                if is_auth_error(&error)
                    && handle_remote_call_auth_failure_and_retry_connect(
                        server_name,
                        &config,
                        &error,
                    )
                    .await?
                {
                    match get_mcp_prompt_once(server_name, prompt_name, arg_names, args).await {
                        Ok(value) => Ok(value),
                        Err((retry_error, _)) => Err(retry_error),
                    }
                } else {
                    Err(error)
                }
            }
            Err((error, None)) => Err(error),
        }
    }

    async fn read_mcp_resource_once(
        server_name: &str,
        uri: &str,
    ) -> Result<Value, (anyhow::Error, Option<ScopedMcpServerConfig>)> {
        let (peer, config) = {
            let clients = CONNECTED_CLIENTS.lock().unwrap();
            let Some(client) = clients.get(server_name) else {
                return Err((
                    anyhow::anyhow!("MCP server \"{server_name}\" is not connected"),
                    None,
                ));
            };
            (client.peer.clone(), client.config.clone())
        };
        // Maps to: CC `ReadMcpResourceTool.call(...)` `client.capabilities?.resources`
        // guard before sending `resources/read`.
        if !peer
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some())
        {
            return Err((
                anyhow::anyhow!("Server \"{server_name}\" does not support resources"),
                Some(config),
            ));
        }
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri.to_string()))
            .await;
        let result = result.map_err(|error| (anyhow::Error::new(error), Some(config)))?;
        serde_json::to_value(result).map_err(|error| (anyhow::Error::new(error), None))
    }

    pub async fn read_mcp_resource(server_name: &str, uri: &str) -> anyhow::Result<Value> {
        match read_mcp_resource_once(server_name, uri).await {
            Ok(value) => Ok(value),
            Err((error, Some(config))) => {
                if is_auth_error(&error)
                    && handle_remote_call_auth_failure_and_retry_connect(
                        server_name,
                        &config,
                        &error,
                    )
                    .await?
                {
                    return match read_mcp_resource_once(server_name, uri).await {
                        Ok(value) => Ok(value),
                        Err((retry_error, Some(retry_config))) => {
                            record_final_remote_call_error(
                                server_name,
                                &retry_config,
                                &retry_error,
                            )?;
                            if is_session_expired_error(&retry_error) {
                                clear_server_cache(server_name, None).await;
                            }
                            Err(retry_error)
                        }
                        Err((retry_error, None)) => Err(retry_error),
                    };
                }
                if is_session_expired_error(&error) {
                    clear_server_cache(server_name, None).await;
                }
                Err(error)
            }
            Err((error, None)) => Err(error),
        }
    }
}

#[cfg(not(feature = "mcp_runtime"))]
mod runtime {
    use super::*;

    pub fn detach_mcp_close_handler(_name: &str, _connection_id: Option<u64>) {}

    pub fn clear_server_cache(
        name: &str,
        _server_ref: Option<&ScopedMcpServerConfig>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let name = name.to_owned();
        async move {
            clear_mcp_server_instructions(&name);
        }
    }

    /// No rmcp client registry to attach the REPL ide-selection sink to.
    pub fn register_ide_selection_sink(
        _sink: async_channel::Sender<crate::hooks::use_ide_selection::IdeSelection>,
    ) {
    }

    pub async fn is_connected_mcp_client(_name: &str) -> bool {
        false
    }

    pub async fn send_custom_notification_to_connected_client(
        _name: &str,
        _method: &str,
        _params: Value,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    pub async fn drain_mcp_connection_callback_observations()
    -> Vec<crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation> {
        Vec::new()
    }

    pub async fn respond_to_mcp_elicitation(
        _server_name: &str,
        _request_id: &str,
        _result: ElicitationResult,
    ) -> bool {
        false
    }

    pub fn clear_mcp_auth_cache() {}

    pub async fn connect_to_server(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        McpConnectionDiscovery::failed(
            name,
            config,
            "mcp_runtime feature is disabled; rmcp client is not compiled",
        )
    }

    pub async fn reconnect_mcp_server_impl(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        connect_to_server(name, config).await
    }

    pub async fn setup_sdk_mcp_clients(
        configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
        _send_mcp_message: crate::services::mcp::sdk_control_transport::SendMcpMessageCallback,
    ) -> SdkMcpClientsSetup {
        let clients = configs
            .iter()
            .map(|(name, config)| {
                McpConnectionDiscovery::failed(
                    name,
                    config,
                    "mcp_runtime feature is disabled; rmcp client is not compiled",
                )
                .server
            })
            .collect::<Vec<_>>();
        SdkMcpClientsSetup {
            clients,
            tools: Vec::new(),
        }
    }

    pub async fn get_mcp_tools_commands_and_resources(
        on_connection_attempt: impl Fn(McpConnectionDiscovery) + Sync,
        configs: &indexmap::IndexMap<String, ScopedMcpServerConfig>,
    ) {
        for (name, config) in crate::utils::process_env::ecmascript_object_entries(configs) {
            let discovery = if super::super::config::is_mcp_server_disabled(name) {
                McpConnectionDiscovery::disabled(name, config)
            } else {
                McpConnectionDiscovery::failed(
                    name,
                    config,
                    "mcp_runtime feature is disabled; rmcp client is not compiled",
                )
            };
            on_connection_attempt(discovery);
        }
    }

    pub async fn refresh_mcp_tools_for_client(_name: &str) -> anyhow::Result<Vec<McpToolSnapshot>> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    pub async fn refresh_mcp_prompts_for_client(
        _name: &str,
    ) -> anyhow::Result<Vec<McpPromptSnapshot>> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    /// No client registry is compiled, so no peer ever declares experimental
    /// capabilities.
    pub async fn experimental_capabilities_by_server()
    -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>> {
        std::collections::BTreeMap::new()
    }

    pub async fn refresh_mcp_resources_for_client(
        _name: &str,
    ) -> anyhow::Result<Vec<ServerResource>> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    pub async fn fetch_mcp_resources_for_client(
        _name: &str,
    ) -> anyhow::Result<Vec<ServerResource>> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    pub async fn delete_resources_fetch_cache_entry(_name: &str) {}

    pub async fn send_channel_permission_request_to_relays(
        _params: &crate::services::mcp::channel_permissions::ChannelPermissionRequestParams,
    ) -> crate::services::mcp::channel_permissions::ChannelPermissionRelaySendReport {
        crate::services::mcp::channel_permissions::ChannelPermissionRelaySendReport {
            enabled: false,
            ..Default::default()
        }
    }

    pub async fn call_mcp_tool(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
    ) -> anyhow::Result<Value> {
        call_mcp_tool_with_meta(server_name, tool_name, args, None).await
    }

    pub async fn call_mcp_tool_with_meta(
        _server_name: &str,
        _tool_name: &str,
        _args: Map<String, Value>,
        _meta: Option<Map<String, Value>>,
    ) -> anyhow::Result<Value> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    // Feature-disabled adapter: preserve the public entry point and the same
    // disabled-runtime error as the other MCP tool-call adapters.
    pub async fn call_mcp_tool_with_elicitation(
        server_name: &str,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
        _handle_elicitation: crate::tool::HandleElicitationCallback,
    ) -> anyhow::Result<Value> {
        call_mcp_tool_with_meta(server_name, tool_name, args, meta).await
    }

    pub async fn get_mcp_prompt_for_command(
        _server_name: &str,
        _prompt_name: &str,
        _arg_names: &[String],
        _args: &str,
    ) -> anyhow::Result<Vec<Value>> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }

    pub async fn read_mcp_resource(_server_name: &str, _uri: &str) -> anyhow::Result<Value> {
        Err(anyhow::anyhow!(
            "mcp_runtime feature is disabled; rmcp client is not compiled"
        ))
    }
}

pub use runtime::{
    call_mcp_tool, call_mcp_tool_with_elicitation, call_mcp_tool_with_meta, clear_mcp_auth_cache,
    clear_server_cache, connect_to_server, delete_resources_fetch_cache_entry,
    detach_mcp_close_handler, experimental_capabilities_by_server, fetch_mcp_resources_for_client,
    get_mcp_prompt_for_command, get_mcp_tools_commands_and_resources, is_connected_mcp_client,
    read_mcp_resource, reconnect_mcp_server_impl, refresh_mcp_prompts_for_client,
    refresh_mcp_resources_for_client, refresh_mcp_tools_for_client, register_ide_selection_sink,
    send_custom_notification_to_connected_client, setup_sdk_mcp_clients,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use image::GenericImageView;
    use std::collections::BTreeMap;
    use std::io::{Read as _, Write as _};

    /// rmcp's Streamable HTTP default follows no redirect, so a server's
    /// custom headers never reach a redirect target; the proxy-aware client
    /// keeps that.
    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn streamable_http_client_follows_no_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env: Vec<_> = ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY", "no_proxy"]
            .into_iter()
            .map(EnvVarGuard::unset)
            .chain([EnvVarGuard::set("NO_PROXY", "127.0.0.1")])
            .collect();
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target.local_addr().unwrap();
        let target_hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit = target_hit.clone();
        let target_server = tokio::spawn(async move {
            let (mut stream, _) = target.accept().await.unwrap();
            hit.store(true, std::sync::atomic::Ordering::SeqCst);
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        });
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_address = origin.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = origin.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).await;
            let reply = format!(
                "HTTP/1.1 302 Found\r\nlocation: http://{target_address}/leak\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });

        let response = super::runtime::streamable_http_client()
            .unwrap()
            .get(format!("http://{origin_address}/mcp"))
            .header("x-api-key", "secret")
            .send()
            .await
            .unwrap();
        server.await.unwrap();
        target_server.abort();
        assert_eq!(response.status(), 302);
        // A followed redirect reaches the target before `send` resolves.
        assert!(!target_hit.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn resource_helper_callback_scope_and_reconnect_match_actual_bun_oracle() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json")).unwrap();
        let mut calls = Vec::new();
        for names in [vec!["a", "b"], vec!["c"]] {
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut completions = Vec::new();
            for name in names {
                let mut discovery = McpConnectionDiscovery::pending(name);
                discovery.server.client.status = McpServerConnectionType::Connected;
                discovery.server.supports_resources = true;
                let mut tool =
                    crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema();
                tool.name = format!("mcp__{name}__tool");
                discovery.tools.push(tool);
                discovery.add_discovery_resource_tools(&added);
                completions.push(serde_json::json!({"name": name, "tools": discovery.tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(), "resourcesUndefined": discovery.resources.is_none()}));
            }
            calls.push(completions);
        }
        assert_eq!(serde_json::json!(calls), oracle["helperCalls"]);
        let mut reconnects = Vec::new();
        for supplied in [
            vec![],
            vec![crate::tools::list_mcp_resources_tool::list_mcp_resources_tool_schema()],
            vec![crate::tools::read_mcp_resource_tool::read_mcp_resource_tool_schema()],
        ] {
            let mut discovery = McpConnectionDiscovery::pending("reconnect");
            discovery.server.client.status = McpServerConnectionType::Connected;
            discovery.server.supports_resources = true;
            let names = supplied
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            discovery.tools = supplied;
            discovery.add_reconnect_resource_tools();
            reconnects.push(serde_json::json!({"supplied": names, "returned": discovery.tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(), "resourcesUndefined": discovery.resources.is_none()}));
        }
        assert_eq!(serde_json::json!(reconnects), oracle["reconnectCases"]);
        let mut auth = McpConnectionDiscovery::pending("auth");
        auth.server.client.status = McpServerConnectionType::NeedsAuth;
        auth.append_resource_tools();
        auth.add_reconnect_resource_tools();
        assert!(
            auth.tools.is_empty(),
            "reconnect non-connected callback supplies no tools"
        );
    }

    #[tokio::test]
    async fn process_batched_matches_official_immediate_completion_and_slot_release() {
        let (events, observed) = async_channel::unbounded();
        let (slow_tx, slow_rx) = tokio::sync::oneshot::channel();
        let (fast_tx, fast_rx) = tokio::sync::oneshot::channel();
        let (next_tx, next_rx) = tokio::sync::oneshot::channel();
        let (remote_tx, remote_rx) = tokio::sync::oneshot::channel();
        let processor = |(name, gate): (&'static str, tokio::sync::oneshot::Receiver<()>)| {
            let events = events.clone();
            async move {
                events.send(("started", name)).await.unwrap();
                gate.await.unwrap();
                // Source processServer calls onConnectionAttempt here, before
                // processBatched (and its sibling pool) finishes.
                events.send(("completed", name)).await.unwrap();
            }
        };
        let scheduler = async {
            tokio::join!(
                process_batched(
                    vec![
                        ("z-slow", slow_rx),
                        ("a-fast", fast_rx),
                        ("b-next", next_rx)
                    ],
                    2,
                    &processor
                ),
                process_batched(vec![("y-remote", remote_rx)], 1, &processor),
            );
        };
        let observer = async {
            let mut started = Vec::new();
            for _ in 0..3 {
                let (kind, name) = observed.recv().await.unwrap();
                assert_eq!(kind, "started");
                started.push(name);
            }
            started.sort();
            assert_eq!(started, ["a-fast", "y-remote", "z-slow"]);
            remote_tx.send(()).unwrap();
            assert_eq!(observed.recv().await.unwrap(), ("completed", "y-remote"));
            fast_tx.send(()).unwrap();
            assert_eq!(observed.recv().await.unwrap(), ("completed", "a-fast"));
            // The third local item starts while the first one remains blocked.
            assert_eq!(observed.recv().await.unwrap(), ("started", "b-next"));
            next_tx.send(()).unwrap();
            assert_eq!(observed.recv().await.unwrap(), ("completed", "b-next"));
            slow_tx.send(()).unwrap();
            assert_eq!(observed.recv().await.unwrap(), ("completed", "z-slow"));
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(scheduler, observer);
        })
        .await
        .expect("completion observers must not await blocked peer servers");
    }

    /// Maps to: CC useIdeSelection.ts:73-83 — a new/changed ide client
    /// identity delivers the CC-shaped reset OBJECT through the REPL sink
    /// captured from the connection-manage registry. Non-ide names never
    /// capture a sink (strict `name === 'ide'` gate, utils/ide.ts:1251).
    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn ide_identity_change_sends_official_reset_object_through_registered_sink() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tx, rx) = async_channel::unbounded();
        crate::services::mcp::use_manage_mcp_connections::set_ide_selection_sink(tx);
        let sink = runtime::ide_selection_sink_for_new_connection("ide")
            .expect("ide connections capture the registered REPL sink");
        assert!(runtime::ide_selection_sink_for_new_connection("docs").is_none());
        assert!(runtime::ide_selection_sink_for_new_connection("IDE").is_none());
        runtime::notify_ide_selection_identity_changed(&sink);
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::hooks::use_ide_selection::ide_selection_reset()
        );
    }

    /// Per-connection sink lifecycle (CC useIdeSelection.ts:148 "No cleanup
    /// needed"): entry removal sends the identity-change reset and then drops
    /// only that connection's sender clone — the registry-held sender stays
    /// alive so the REPL channel keeps serving future ide connections.
    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn per_connection_ide_sink_drop_releases_only_that_connections_sender() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (tx, rx) = async_channel::unbounded::<crate::hooks::use_ide_selection::IdeSelection>();
        crate::services::mcp::use_manage_mcp_connections::set_ide_selection_sink(tx.clone());
        drop(tx);
        let entry_sink = runtime::ide_selection_sink_for_new_connection("ide")
            .expect("ide connections capture the registered REPL sink");
        assert_eq!(rx.sender_count(), 2, "registry sender + per-entry clone");
        // Close-watcher / clear_server_cache removal path: reset, then drop.
        runtime::notify_ide_selection_identity_changed(&entry_sink);
        drop(entry_sink);
        assert_eq!(
            rx.sender_count(),
            1,
            "registry sender persists for future connections"
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::hooks::use_ide_selection::ide_selection_reset()
        );
        assert!(
            rx.try_recv().is_err(),
            "no further deliveries after the entry sink drops"
        );
    }

    fn spawn_count_tokens_server(
        response_body: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut expected_length = None;
            loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if expected_length.is_none() {
                    if let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or_default();
                        expected_length = Some(header_end + 4 + content_length);
                    }
                }
                if expected_length.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), worker)
    }

    fn state_with_clients(clients: Vec<McpServerSnapshot>) -> McpState {
        let mut state = McpState {
            clients,
            ..McpState::default()
        };
        refresh_flat_mcp_capabilities(&mut state);
        state
    }

    #[test]
    fn mcp_tool_input_classifier_encoding_matches_official_shape() {
        let mut input = Map::new();
        input.insert("issue".to_string(), Value::String("123".to_string()));
        input.insert("dryRun".to_string(), Value::Bool(true));
        assert_eq!(
            mcp_tool_input_to_auto_classifier_input(&input, "create_issue"),
            "issue=123 dryRun=true"
        );
        assert_eq!(
            mcp_tool_input_to_auto_classifier_input(&Map::new(), "create_issue"),
            "create_issue"
        );
    }

    #[test]
    fn mcp_tool_use_id_meta_matches_official_call_payload() {
        assert_eq!(
            mcp_tool_use_id_meta("toolu_123").get("claudecode/toolUseId"),
            Some(&Value::String("toolu_123".to_string()))
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn mcp_call_tool_request_params_include_official_meta_payload() {
        let params = runtime::call_tool_request_params(
            "search",
            Map::from_iter([("query".to_string(), Value::String("docs".to_string()))]),
            Some(mcp_tool_use_id_meta("toolu_123")),
        );
        let encoded = serde_json::to_value(params).expect("params serialize");
        assert_eq!(encoded["name"], Value::String("search".to_string()));
        assert_eq!(
            encoded["arguments"]["query"],
            Value::String("docs".to_string())
        );
        assert_eq!(
            encoded["_meta"]["claudecode/toolUseId"],
            Value::String("toolu_123".to_string())
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn mcp_connection_batch_size_env_matches_official_defaults() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _local = EnvVarGuard::unset("MCP_SERVER_CONNECTION_BATCH_SIZE");
        let _remote = EnvVarGuard::unset("MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE");

        assert_eq!(runtime::get_mcp_server_connection_batch_size(), 3);
        assert_eq!(runtime::get_remote_mcp_server_connection_batch_size(), 20);

        crate::utils::process_env::set("MCP_SERVER_CONNECTION_BATCH_SIZE", "5");
        crate::utils::process_env::set("MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE", "42");
        assert_eq!(runtime::get_mcp_server_connection_batch_size(), 5);
        assert_eq!(runtime::get_remote_mcp_server_connection_batch_size(), 42);

        crate::utils::process_env::set("MCP_SERVER_CONNECTION_BATCH_SIZE", "0");
        crate::utils::process_env::set("MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE", "bad");
        assert_eq!(runtime::get_mcp_server_connection_batch_size(), 3);
        assert_eq!(runtime::get_remote_mcp_server_connection_batch_size(), 20);
    }

    #[test]
    fn mcp_tool_timeout_env_falls_back_to_official_default() {
        assert_eq!(
            get_mcp_tool_timeout_ms_from_env(|_| None),
            DEFAULT_MCP_TOOL_TIMEOUT_MS
        );
        assert_eq!(
            get_mcp_tool_timeout_ms_from_env(
                |key| (key == "MCP_TOOL_TIMEOUT").then(|| "42".to_string())
            ),
            42
        );
        assert_eq!(
            get_mcp_tool_timeout_ms_from_env(
                |key| (key == "MCP_TOOL_TIMEOUT").then(|| "0".to_string())
            ),
            DEFAULT_MCP_TOOL_TIMEOUT_MS
        );
    }

    #[test]
    fn are_mcp_configs_equal_ignores_scope_but_compares_connection_fields() {
        let mut env = BTreeMap::new();
        env.insert("TOKEN".to_string(), "one".to_string());
        let mut headers = BTreeMap::new();
        headers.insert("X-Test".to_string(), "yes".to_string());
        let base = ScopedMcpServerConfig {
            name: None,
            scope: super::super::types::ConfigScope::Project,
            transport: Transport::Http,
            command: None,
            args: Vec::new(),
            env,
            url: Some("https://mcp.example.test".to_string()),
            headers,
            headers_helper: Some("helper".to_string()),
            oauth: Some(serde_json::json!({ "enabled": true })),
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: Some("token".to_string()),
            id: Some("server-id".to_string()),
            plugin_source: Some("plugin".to_string()),
        };

        let mut moved_scope = base.clone();
        moved_scope.scope = super::super::types::ConfigScope::User;
        assert!(are_mcp_configs_equal(&base, &moved_scope));

        let mut different_transport = base.clone();
        different_transport.transport = Transport::Sse;
        assert!(!are_mcp_configs_equal(&base, &different_transport));

        let mut different_url = base.clone();
        different_url.url = Some("https://other.example.test".to_string());
        assert!(!are_mcp_configs_equal(&base, &different_url));

        let mut different_env = base.clone();
        different_env
            .env
            .insert("TOKEN".to_string(), "two".to_string());
        assert!(!are_mcp_configs_equal(&base, &different_env));
    }

    #[test]
    fn infer_compact_schema_matches_official_depth_and_suffix_rules() {
        assert_eq!(infer_compact_schema(&Value::Null, 2), "null");
        assert_eq!(infer_compact_schema(&serde_json::json!([]), 2), "[]");
        assert_eq!(
            infer_compact_schema(
                &serde_json::json!({
                    "title": "Issue",
                    "items": [{"id": 1, "nested": {"ok": true}}],
                    "empty": []
                }),
                3,
            ),
            "{title: string, items: [{id: number, nested: {...}}], empty: []}"
        );
        assert_eq!(
            infer_compact_schema(
                &serde_json::json!({
                    "a": 1, "b": 2, "c": 3, "d": 4, "e": 5,
                    "f": 6, "g": 7, "h": 8, "i": 9, "j": 10, "k": 11
                }),
                2,
            ),
            "{a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, ...}"
        );
    }

    #[test]
    fn transform_mcp_result_matches_official_priority_and_schemas() {
        let tool_result = transform_mcp_result(
            &serde_json::json!({
                "toolResult": 123,
                "structuredContent": {"ignored": true}
            }),
            "lookup",
            "docs",
        )
        .expect("toolResult result");
        assert_eq!(
            tool_result.result_type,
            crate::utils::mcp_output_storage::McpResultType::ToolResult
        );
        assert_eq!(tool_result.content, Value::String("123".to_string()));
        assert_eq!(tool_result.schema, None);

        let structured = transform_mcp_result(
            &serde_json::json!({
                "structuredContent": {"title": "Issue", "items": [{"id": 1}]}
            }),
            "lookup",
            "docs",
        )
        .expect("structured result");
        assert_eq!(
            structured.result_type,
            crate::utils::mcp_output_storage::McpResultType::StructuredContent
        );
        assert_eq!(
            structured.content,
            Value::String(r#"{"title":"Issue","items":[{"id":1}]}"#.to_string())
        );
        assert_eq!(
            structured.schema.as_deref(),
            Some("{title: string, items: [{...}]}")
        );

        let structured_null = transform_mcp_result(
            &serde_json::json!({ "structuredContent": null }),
            "lookup",
            "docs",
        )
        .expect("null structured content still matches official !== undefined check");
        assert_eq!(structured_null.content, Value::String("null".to_string()));
        assert_eq!(structured_null.schema.as_deref(), Some("null"));
    }

    #[test]
    fn transform_mcp_content_array_matches_official_text_resource_projection() {
        let transformed = transform_mcp_result(
            &serde_json::json!({
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "resource_link", "name": "doc", "uri": "file://a", "description": "desc"},
                    {"type": "resource", "resource": {"uri": "mem://note", "text": "body"}}
                ]
            }),
            "lookup",
            "docs",
        )
        .expect("content array result");

        assert_eq!(
            transformed.result_type,
            crate::utils::mcp_output_storage::McpResultType::ContentArray
        );
        assert_eq!(
            transformed.schema.as_deref(),
            Some("[{type: string, text: string}]")
        );
        assert_eq!(
            transformed_mcp_result_summary(&transformed),
            "hello\n[Resource link: doc] file://a (desc)\n[Resource from docs at mem://note] body"
        );
    }

    fn test_png_base64(width: u32, height: u32) -> String {
        use base64::Engine as _;
        use image::ImageEncoder;

        let rgba = image::RgbaImage::from_pixel(width, height, image::Rgba([64, 80, 96, 255]));
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn transform_result_content_resizes_mcp_images_like_official() {
        let transformed = transform_result_content(
            &serde_json::json!({
                "type": "image",
                "data": test_png_base64(2500, 500),
                "mimeType": "image/png"
            }),
            "docs",
        )
        .expect("image transform");

        assert_eq!(transformed.len(), 1);
        assert_eq!(transformed[0]["type"], Value::String("image".to_string()));
        assert_eq!(
            transformed[0]["source"]["media_type"],
            Value::String("image/png".to_string())
        );
        let data = transformed[0]["source"]["data"].as_str().unwrap();
        let bytes = decode_mcp_base64(data).expect("resized image base64");
        let image = image::load_from_memory(&bytes).expect("resized image decodes");
        assert_eq!(image.dimensions(), (2000, 400));
    }

    #[test]
    fn transform_mcp_result_propagates_image_resize_errors_like_official() {
        let error = transform_mcp_result(
            &serde_json::json!({
                "content": [{"type": "image", "data": "", "mimeType": "image/png"}]
            }),
            "screenshot",
            "docs",
        )
        .expect_err("empty MCP image should fail before API call");

        assert!(error.to_string().contains("Image file is empty (0 bytes)"));
    }

    #[test]
    fn transform_mcp_result_rejects_unexpected_response_like_official() {
        let error =
            transform_mcp_result(&serde_json::json!({"unexpected": true}), "lookup", "docs")
                .expect_err("unexpected shape should fail");
        assert_eq!(
            error.to_string(),
            "MCP server \"docs\" tool \"lookup\": unexpected response format"
        );
    }

    #[tokio::test]
    async fn truncate_mcp_content_compresses_image_blocks_to_remaining_budget_like_official() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _max = EnvVarGuard::set("MAX_MCP_OUTPUT_TOKENS", "100");
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": test_png_base64(1, 1)
                }
            }
        ]);

        let truncated = crate::utils::mcp_validation::truncate_mcp_content(&content).await;
        let blocks = truncated.as_array().expect("truncated content array");

        assert_eq!(blocks[0]["type"], Value::String("image".to_string()));
        assert_eq!(
            blocks[0]["source"]["media_type"],
            Value::String("image/png".to_string())
        );
        assert!(blocks[0]["source"]["data"].as_str().unwrap().len() <= 400);
        assert!(
            blocks
                .last()
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .unwrap()
                .contains("[OUTPUT TRUNCATED - exceeded 100 token limit]")
        );
    }

    #[tokio::test]
    async fn process_mcp_result_persists_large_non_image_output_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = crate::bootstrap::state::get_original_cwd();
        let previous_session_id = crate::bootstrap::state::get_session_id();
        let root =
            std::env::temp_dir().join(format!("cometix-mcp-large-output-{}", uuid::Uuid::new_v4()));
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(&root);
        crate::bootstrap::state::set_original_cwd("/tmp/cometix-project");
        crate::bootstrap::state::set_session_id("session-mcp-large-output-test");
        let (endpoint, server) = spawn_count_tokens_server(r#"{"inputTokens":999}"#);
        let _config_home = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _max_tokens = EnvVarGuard::set("MAX_MCP_OUTPUT_TOKENS", "5");
        let _large_output = EnvVarGuard::set("ENABLE_MCP_LARGE_OUTPUT_FILES", "1");
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _skip_auth = EnvVarGuard::set("CLAUDE_CODE_SKIP_BEDROCK_AUTH", "1");
        let _endpoint = EnvVarGuard::set("AWS_ENDPOINT_URL_BEDROCK_RUNTIME", &endpoint);
        let _model = EnvVarGuard::set(
            "ANTHROPIC_MODEL",
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
        );

        let result = process_mcp_result(
            &serde_json::json!({ "structuredContent": { "items": ["abcdef".repeat(80)] } }),
            "search",
            "docs server",
        )
        .await
        .expect("processed mcp result");
        server.join().unwrap();
        let instructions = result.content.as_str().expect("instructions string");
        assert!(instructions.contains("Output has been saved to"));
        assert!(instructions.contains("Format: JSON with schema:"));
        assert!(instructions.contains("You MUST read the content from the file"));

        let saved_path = crate::utils::tool_result_storage::get_tool_results_dir()
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(
            saved_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mcp-docs_server-search-")
        );
        assert_eq!(
            saved_path.extension().and_then(|ext| ext.to_str()),
            Some("txt")
        );
        assert!(
            std::fs::read_to_string(&saved_path)
                .unwrap()
                .contains("abcdef")
        );

        crate::bootstrap::state::set_original_cwd(previous_cwd);
        crate::bootstrap::state::set_session_id(previous_session_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_prompt_command_projection_matches_official_command_shape() {
        let arg_names = vec!["channel".to_string(), "topic".to_string()];
        assert_eq!(
            mcp_prompt_command_name("My Server", "daily-report"),
            "mcp__My_Server__daily-report"
        );
        assert_eq!(
            mcp_prompt_user_facing_name("My Server", "daily-report"),
            "My Server:daily-report (MCP)"
        );
        assert_eq!(
            mcp_prompt_arguments_from_args(&arg_names, "eng roadmap")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                ("channel".to_string(), Value::String("eng".to_string())),
                ("topic".to_string(), Value::String("roadmap".to_string()))
            ]
        );
        assert_eq!(
            mcp_prompt_arguments_from_args(&arg_names, "").get("channel"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn connection_update_writes_flat_mcp_commands_for_connected_clients() {
        let state = state_with_clients(vec![
            McpServerSnapshot {
                connection_id: None,
                client: McpClientSnapshot {
                    name: "docs".to_string(),
                    status: McpServerConnectionType::Connected,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: vec![McpPromptSnapshot {
                    name: "summarize".to_string(),
                    description: Some("Summarize docs".to_string()),
                    arg_names: vec!["path".to_string()],
                }],
                resources: Vec::new(),
            },
            McpServerSnapshot {
                connection_id: None,
                client: McpClientSnapshot {
                    name: "pending".to_string(),
                    status: McpServerConnectionType::Pending,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: vec![McpPromptSnapshot {
                    name: "hidden".to_string(),
                    description: None,
                    arg_names: Vec::new(),
                }],
                resources: Vec::new(),
            },
        ]);

        assert_eq!(state.commands.len(), 1);
        assert_eq!(state.commands[0].name, "mcp__docs__summarize");
        assert_eq!(state.commands[0].description, "Summarize docs");
        assert!(state.commands[0].has_user_specified_description);
        assert_eq!(
            state.commands[0].user_facing_name.as_deref(),
            Some("docs:summarize (MCP)")
        );
        assert_eq!(state.commands[0].arg_names, vec!["path"]);
        assert_eq!(
            state.commands[0].source,
            crate::commands::CommandSource::Mcp
        );
        assert_eq!(
            resolve_mcp_prompt_command_invocation("mcp__docs__summarize", &state),
            Some((
                "docs".to_string(),
                McpPromptSnapshot {
                    name: "summarize".to_string(),
                    description: Some("Summarize docs".to_string()),
                    arg_names: vec!["path".to_string()],
                }
            ))
        );
        assert_eq!(
            resolve_mcp_prompt_command_invocation("mcp__pending__hidden", &state),
            None
        );
    }

    #[test]
    fn resource_capability_injects_helpers_even_when_resource_list_is_empty() {
        let mut server = McpConnectionDiscovery::pending("empty-resources").server;
        server.client.status = McpServerConnectionType::Connected;
        server.supports_resources = true;
        let state = state_with_clients(vec![server]);
        let names = state
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(
            &crate::tools::list_mcp_resources_tool::prompt::LIST_MCP_RESOURCES_TOOL_NAME
        ));
        assert!(
            names.contains(
                &crate::tools::read_mcp_resource_tool::prompt::READ_MCP_RESOURCE_TOOL_NAME
            )
        );
        assert!(state.resources.is_empty());
    }

    #[test]
    fn mcp_prompt_content_blocks_summary_preserves_text_and_json_blocks() {
        assert_eq!(
            mcp_prompt_content_blocks_summary(&[
                serde_json::json!({"type":"text","text":"first"}),
                serde_json::json!({"type":"image","source":{"type":"base64","data":"abc"}})
            ]),
            "first\n{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"data\":\"abc\"}}"
        );
    }

    #[test]
    fn runtime_mcp_state_projects_model_tools_and_resolves_raw_invocation() {
        let state = state_with_clients(vec![
            McpServerSnapshot {
                connection_id: None,
                client: McpClientSnapshot {
                    name: "GitHub Server".to_string(),
                    status: McpServerConnectionType::Connected,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: Some("1.0.0".to_string()),
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: vec![McpToolSnapshot {
                    name: "Create Issue".to_string(),
                    display_name: Some("Create Issue".to_string()),
                    description: Some("Create a GitHub issue".to_string()),
                    input_schema: serde_json::json!({"type":"object"}),
                    read_only_hint: false,
                    destructive_hint: false,
                    open_world_hint: true,
                }],
                prompts: Vec::new(),
                resources: Vec::new(),
            },
            McpServerSnapshot {
                connection_id: None,
                client: McpClientSnapshot {
                    name: "pending".to_string(),
                    status: McpServerConnectionType::Pending,
                    reconnect_attempt: None,
                    max_reconnect_attempts: None,
                    ide_name: None,
                    server_version: None,
                    error: None,
                },
                config: None,
                supports_resources: false,
                tools: Vec::new(),
                prompts: Vec::new(),
                resources: Vec::new(),
            },
        ]);

        let tools = &state.tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__GitHub_Server__Create_Issue");
        assert!(tools[0].is_mcp);
        assert_eq!(tools[0].description, "Create a GitHub issue");
        assert!(has_pending_mcp_servers(&state));
        assert_eq!(
            resolve_mcp_tool_invocation("mcp__GitHub_Server__Create_Issue", &state),
            Some(("GitHub Server".to_string(), "Create Issue".to_string()))
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn setup_sdk_mcp_clients_emits_control_messages_and_reports_failures() {
        let mut configs = indexmap::IndexMap::new();
        configs.insert(
            "sdk-server".to_string(),
            ScopedMcpServerConfig {
                name: Some("sdk-server".to_string()),
                scope: crate::services::mcp::types::ConfigScope::User,
                transport: Transport::Sdk,
                command: None,
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
            },
        );
        let seen_servers = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_for_callback = std::sync::Arc::clone(&seen_servers);
        let callback: crate::services::mcp::sdk_control_transport::SendMcpMessageCallback =
            std::sync::Arc::new(move |server_name, message| {
                let seen_for_callback = std::sync::Arc::clone(&seen_for_callback);
                Box::pin(async move {
                    seen_for_callback.lock().unwrap().push(server_name);
                    let id = message.get("id").cloned().unwrap_or(Value::Null);
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": "sdk failed" }
                    }))
                })
            });

        let setup = setup_sdk_mcp_clients(&configs, callback).await;

        assert_eq!(setup.clients.len(), 1);
        assert_eq!(setup.clients[0].client.name, "sdk-server");
        assert_eq!(
            setup.clients[0].client.status,
            McpServerConnectionType::Failed
        );
        assert!(
            setup.clients[0]
                .client
                .error
                .as_deref()
                .is_some_and(|error| error.contains("sdk failed"))
        );
        assert_eq!(
            setup.clients[0].config.as_ref().map(|config| config.scope),
            Some(crate::services::mcp::types::ConfigScope::User)
        );
        assert!(setup.tools.is_empty());
        assert_eq!(seen_servers.lock().unwrap().as_slice(), &["sdk-server"]);
    }

    #[cfg(feature = "mcp_runtime")]
    #[tokio::test]
    async fn setup_sdk_mcp_clients_connects_via_control_transport_and_fetches_tools() {
        let mut configs = indexmap::IndexMap::new();
        configs.insert(
            "sdk-connected".to_string(),
            ScopedMcpServerConfig {
                name: Some("sdk-connected".to_string()),
                scope: crate::services::mcp::types::ConfigScope::User,
                transport: Transport::Sdk,
                command: None,
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
            },
        );
        let methods = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let methods_for_callback = std::sync::Arc::clone(&methods);
        let callback: crate::services::mcp::sdk_control_transport::SendMcpMessageCallback =
            std::sync::Arc::new(move |server_name, message| {
                let methods_for_callback = std::sync::Arc::clone(&methods_for_callback);
                Box::pin(async move {
                    assert_eq!(server_name, "sdk-connected");
                    let method = message
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    methods_for_callback.lock().unwrap().push(method.clone());
                    let id = message.get("id").cloned().unwrap_or(Value::Null);
                    match method.as_str() {
                        "initialize" => Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": "2025-11-25",
                                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                                "serverInfo": { "name": "sdk-connected", "version": "1.2.3" }
                            }
                        })),
                        "tools/list" => Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": [{
                                    "name": "echo",
                                    "description": "Echo input",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "text": { "type": "string" }
                                        }
                                    }
                                }]
                            }
                        })),
                        _ if id.is_null() => Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/ack"
                        })),
                        _ => Ok(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": "unknown method" }
                        })),
                    }
                })
            });

        let setup = setup_sdk_mcp_clients(&configs, callback.clone()).await;

        assert_eq!(setup.clients.len(), 1);
        let client = &setup.clients[0];
        assert_eq!(client.client.name, "sdk-connected");
        assert_eq!(client.client.status, McpServerConnectionType::Connected);
        assert_eq!(client.client.server_version.as_deref(), Some("1.2.3"));
        assert_eq!(
            client.config.as_ref().map(|config| config.scope),
            Some(crate::services::mcp::types::ConfigScope::Dynamic)
        );
        assert_eq!(client.tools.len(), 1);
        assert_eq!(client.tools[0].name, "echo");
        assert_eq!(setup.tools.len(), 1);
        assert_eq!(setup.tools[0].name, "mcp__sdk-connected__echo");
        let methods = methods.lock().unwrap().clone();
        assert!(methods.contains(&"initialize".to_string()));
        assert!(methods.contains(&"tools/list".to_string()));
        // CC setupSdkMcpClients only discovers tools, even if resources exist.
        assert!(client.supports_resources);
        assert!(
            !methods
                .iter()
                .any(|method| method == "prompts/list" || method == "resources/list")
        );
        // Source cleanup captures the memo instance before its first await.
        // A stale snapshot must neither detach nor delete a replacement client.
        let replacement = setup_sdk_mcp_clients(&configs, callback.clone()).await;
        let replacement_id = replacement.clients[0].connection_id;
        assert_ne!(client.connection_id, replacement_id);
        detach_mcp_close_handler("sdk-connected", client.connection_id);
        assert!(runtime::CONNECTED_CLIENTS.lock().unwrap()["sdk-connected"].on_close_enabled);
        let delayed_clear =
            clear_server_cache("sdk-connected", replacement.clients[0].config.as_ref());
        let latest = setup_sdk_mcp_clients(&configs, callback).await;
        delayed_clear.await;
        assert!(is_connected_mcp_client("sdk-connected").await);
        detach_mcp_close_handler("sdk-connected", latest.clients[0].connection_id);
        assert!(!runtime::CONNECTED_CLIENTS.lock().unwrap()["sdk-connected"].on_close_enabled);
        let mut mismatched_config = latest.clients[0].config.clone().unwrap();
        mismatched_config.url = Some("https://different.invalid".into());
        clear_server_cache("sdk-connected", Some(&mismatched_config)).await;
        assert!(is_connected_mcp_client("sdk-connected").await);
        clear_server_cache("sdk-connected", None).await;
    }

    #[test]
    fn sdk_mcp_tools_can_skip_prefix_like_official_env_gate() {
        let sdk_config = ScopedMcpServerConfig {
            name: Some("sdk-server".to_string()),
            scope: crate::services::mcp::types::ConfigScope::User,
            transport: Transport::Sdk,
            command: None,
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
        };
        let mut state = state_with_clients(vec![McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: "sdk server".to_string(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: Some("1.0.0".to_string()),
                error: None,
            },
            config: Some(sdk_config),
            supports_resources: false,
            tools: vec![McpToolSnapshot {
                name: "Read".to_string(),
                display_name: None,
                description: Some("SDK read".to_string()),
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            }],
            prompts: Vec::new(),
            resources: Vec::new(),
        }]);

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let key = "CLAUDE_AGENT_SDK_MCP_NO_PREFIX";
        let _prefix = EnvVarGuard::unset(key);
        refresh_flat_mcp_capabilities(&mut state);

        // Maps to: CC `client.ts:1774` — `mcpInfo` holds the unnormalized server
        // and tool names and is set regardless of `skipPrefix`.
        let expected_mcp_info = Some(crate::types::tools::McpToolInfo {
            server_name: "sdk server".to_string(),
            tool_name: "Read".to_string(),
        });

        let prefixed = state.tools.clone();
        assert_eq!(prefixed[0].name, "mcp__sdk_server__Read");
        assert_eq!(prefixed[0].mcp_info, expected_mcp_info);
        assert_eq!(
            resolve_mcp_tool_invocation("mcp__sdk_server__Read", &state),
            Some(("sdk server".to_string(), "Read".to_string()))
        );

        crate::utils::process_env::set(key, "1");
        refresh_flat_mcp_capabilities(&mut state);
        let unprefixed = state.tools.clone();
        assert_eq!(unprefixed[0].name, "Read");
        assert_eq!(unprefixed[0].mcp_info, expected_mcp_info);
        assert_eq!(
            crate::services::mcp::mcp_string_utils::get_tool_name_for_permission_check(
                &unprefixed[0].name,
                unprefixed[0].mcp_info.as_ref(),
            ),
            "mcp__sdk_server__Read"
        );
        assert_eq!(
            resolve_mcp_tool_invocation("Read", &state),
            Some(("sdk server".to_string(), "Read".to_string()))
        );
    }

    #[test]
    fn ide_mcp_tools_are_filtered_like_official_fetch_tools_for_client() {
        let state = state_with_clients(vec![McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: "ide".to_string(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: Some("IDE".to_string()),
                server_version: Some("1.0.0".to_string()),
                error: None,
            },
            config: None,
            supports_resources: false,
            tools: vec![
                McpToolSnapshot {
                    name: "executeCode".to_string(),
                    display_name: None,
                    description: Some("Execute code".to_string()),
                    input_schema: serde_json::json!({"type":"object"}),
                    read_only_hint: false,
                    destructive_hint: false,
                    open_world_hint: false,
                },
                McpToolSnapshot {
                    name: "getDiagnostics".to_string(),
                    display_name: None,
                    description: Some("Get diagnostics".to_string()),
                    input_schema: serde_json::json!({"type":"object"}),
                    read_only_hint: true,
                    destructive_hint: false,
                    open_world_hint: false,
                },
                McpToolSnapshot {
                    name: "openFile".to_string(),
                    display_name: None,
                    description: Some("Open file".to_string()),
                    input_schema: serde_json::json!({"type":"object"}),
                    read_only_hint: true,
                    destructive_hint: false,
                    open_world_hint: false,
                },
            ],
            prompts: Vec::new(),
            resources: Vec::new(),
        }]);

        let tools = state
            .tools
            .iter()
            .cloned()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(
            tools,
            vec![
                "mcp__ide__executeCode".to_string(),
                "mcp__ide__getDiagnostics".to_string()
            ]
        );
        assert_eq!(
            resolve_mcp_tool_invocation("mcp__ide__executeCode", &state),
            Some(("ide".to_string(), "executeCode".to_string()))
        );
        assert_eq!(
            resolve_mcp_tool_invocation("mcp__ide__openFile", &state),
            None
        );
    }

    #[test]
    fn needs_auth_state_projects_authenticate_pseudo_tool_like_official() {
        let config = ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::User,
            transport: Transport::Http,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("https://example.com/mcp".to_string()),
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: Some(serde_json::json!({"clientId":"client"})),
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        };
        let discovery = McpConnectionDiscovery::needs_auth("docs", &config);
        let state = state_with_clients(vec![discovery.server]);

        let tools = &state.tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__docs__authenticate");
        assert!(tools[0].is_mcp);
        assert!(tools[0].description.contains("requires authentication"));
        assert_eq!(
            resolve_mcp_tool_invocation("mcp__docs__authenticate", &state),
            Some(("docs".to_string(), "authenticate".to_string()))
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn elicitation_notification_messages_match_official_hooks() {
        assert_eq!(
            runtime::elicitation_complete_notification_message("docs", "elicit-1"),
            "MCP server \"docs\" confirmed elicitation elicit-1 complete"
        );
        assert_eq!(
            runtime::elicitation_response_notification_message("docs", "decline"),
            "Elicitation response for server \"docs\": decline"
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn url_elicitation_required_error_parsing_matches_official_validation() {
        let error = anyhow::Error::new(rmcp::ErrorData::url_elicitation_required(
            "open url",
            Some(serde_json::json!({
                "elicitations": [
                    {
                        "mode": "url",
                        "url": "https://example.com/auth",
                        "elicitationId": "elicit-1",
                        "message": "Authorize"
                    },
                    { "mode": "url", "url": 42, "elicitationId": "bad", "message": "bad" },
                    { "mode": "form", "message": "ignored" }
                ]
            })),
        ));

        assert!(runtime::is_url_elicitation_required_error(&error));
        let elicitations = runtime::url_elicitations_from_error(&error);
        assert_eq!(elicitations.len(), 1);
        match &elicitations[0] {
            rmcp::model::ElicitRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => {
                assert_eq!(message, "Authorize");
                assert_eq!(url, "https://example.com/auth");
                assert_eq!(elicitation_id, "elicit-1");
            }
            _ => panic!("expected URL elicitation"),
        }
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn channel_custom_notification_emits_callback_observation() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let mut capabilities = rmcp::model::ServerCapabilities::default();
                capabilities.experimental = Some(BTreeMap::from([(
                    crate::services::mcp::channel_notification::CHANNEL_EXPERIMENTAL_CAPABILITY
                        .to_string(),
                    Map::new(),
                )]));
                let peer_info = rmcp::model::ServerInfo::new(capabilities);

                let emitted = runtime::emit_channel_message_event_from_custom_notification(
                    "slack".to_string(),
                    rmcp::model::CustomNotification::new(
                        crate::services::mcp::channel_notification::CHANNEL_MESSAGE_NOTIFICATION_METHOD,
                        Some(serde_json::json!({
                            "content": "hello",
                            "meta": { "thread_ts": "123" }
                        })),
                    ),
                    Some(&peer_info),
                )
                .await;
                assert!(emitted);

                let events = runtime::drain_mcp_connection_callback_observations().await;
                match events.as_slice() {
                    [crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ChannelMessageReceived {
                        name,
                        content,
                        meta,
                        has_channel_capability,
                        plugin_source,
                    }] => {
                        assert_eq!(name, "slack");
                        assert_eq!(content, "hello");
                        assert_eq!(
                            meta.as_ref().and_then(|meta| meta.get("thread_ts")),
                            Some(&"123".to_string())
                        );
                        assert!(*has_channel_capability);
                        assert_eq!(plugin_source, &None);
                    }
                    other => panic!("expected one channel event, got {other:?}"),
                }
            });
    }

    #[test]
    fn channel_permission_relay_send_is_disabled_by_default() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let params =
                    crate::services::mcp::channel_permissions::channel_permission_request_params(
                        "toolu_123",
                        "Bash",
                        "Run command?",
                        &serde_json::json!({ "command": "echo hi" }),
                    );
                let report = send_channel_permission_request_to_relays(&params).await;
                assert!(!report.enabled);
                assert_eq!(report.attempted, 0);
                assert_eq!(report.sent, 0);
                assert_eq!(report.failed, 0);
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn channel_permission_custom_notification_emits_callback_observation() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let mut capabilities = rmcp::model::ServerCapabilities::default();
                capabilities.experimental = Some(BTreeMap::from([(
                    crate::services::mcp::channel_notification::CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY
                        .to_string(),
                    Map::new(),
                )]));
                let peer_info = rmcp::model::ServerInfo::new(capabilities);

                let emitted = runtime::emit_channel_permission_event_from_custom_notification(
                    "telegram".to_string(),
                    rmcp::model::CustomNotification::new(
                        crate::services::mcp::channel_notification::CHANNEL_PERMISSION_METHOD,
                        Some(serde_json::json!({
                            "request_id": "TbXkQ",
                            "behavior": "deny"
                        })),
                    ),
                    Some(&peer_info),
                )
                .await;
                assert!(emitted);

                let events = runtime::drain_mcp_connection_callback_observations().await;
                match events.as_slice() {
                    [crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ChannelPermissionReceived {
                        name,
                        request_id,
                        behavior,
                        has_permission_capability,
                    }] => {
                        assert_eq!(name, "telegram");
                        assert_eq!(request_id, "TbXkQ");
                        assert_eq!(
                            *behavior,
                            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Deny
                        );
                        assert!(*has_permission_capability);
                    }
                    other => panic!("expected one channel permission event, got {other:?}"),
                }
            });
    }

    /// Maps to: CC `callMCPToolWithUrlElicitationRetry` client.ts:2944-2947 —
    /// with a handleElicitation leg (print/SDK mode) the resolution delegates
    /// to it instead of queuing an ElicitationRequestEvent for the dialog.
    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn url_elicitation_prefers_the_sdk_handler_over_the_queue() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let elicitation =
                    |id: &str| rmcp::model::ElicitRequestParams::UrlElicitationParams {
                        meta: None,
                        message: "Authorize".to_string(),
                        url: "https://example.com/auth".to_string(),
                        elicitation_id: id.to_string(),
                    };

                // Accepting handler: retry proceeds (None) without any queued
                // dialog event.
                let accept_handler = crate::tool::HandleElicitationCallback::new(|_, _| {
                    Box::pin(async { ElicitationResult::new(ElicitationAction::Accept) })
                });
                let result = runtime::process_url_elicitation_required(
                    "sdk-elicit-server",
                    "login",
                    vec![elicitation("sdk-accept")],
                    accept_handler,
                )
                .await;
                assert!(result.is_none(), "accept lets the retry proceed");
                let events = runtime::drain_mcp_connection_callback_observations().await;
                assert!(
                    events.is_empty(),
                    "the SDK handler bypasses the dialog queue: {events:?}"
                );

                // Declining handler: the tool result carries CC's copy.
                let decline_handler = crate::tool::HandleElicitationCallback::new(|_, _| {
                    Box::pin(async { ElicitationResult::new(ElicitationAction::Decline) })
                });
                let result = runtime::process_url_elicitation_required(
                    "sdk-elicit-server",
                    "login",
                    vec![elicitation("sdk-decline")],
                    decline_handler,
                )
                .await
                .expect("decline resolves the tool call");
                let text = result["content"][0]["text"].as_str().unwrap();
                assert!(
                    text.contains("URL elicitation was declined by the user"),
                    "official decline copy, got: {text}"
                );
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn url_elicitation_required_retry_queue_accept_and_decline_paths() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = runtime::drain_mcp_connection_callback_observations().await;

                const SERVER_ACCEPT: &str = "docs-url-elicit-accept";
                let accept = rmcp::model::ElicitRequestParams::UrlElicitationParams {
                    meta: None,
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: "elicit-accept".to_string(),
                };
                let accept_handle = tokio::spawn(runtime::process_url_elicitation_required(
                    SERVER_ACCEPT,
                    "login",
                    vec![accept],
                    crate::tool::HandleElicitationCallback::default(),
                ));
                tokio::task::yield_now().await;
                let events = runtime::drain_mcp_connection_callback_observations().await;
                let request_id = match events.as_slice() {
                    [crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ElicitationRequested { event }] => {
                        assert_eq!(event.server_name, SERVER_ACCEPT);
                        assert_eq!(event.request_id, "error-elicit-elicit-accept");
                        assert!(event.waiting_state.is_some());
                        event.request_id.clone()
                    }
                    other => panic!("expected one URL elicitation event, got {other:?}"),
                };
                assert!(
                    runtime::respond_to_mcp_elicitation(
                        SERVER_ACCEPT,
                        &request_id,
                        ElicitationResult::new(ElicitationAction::Accept),
                    )
                    .await
                );
                assert!(accept_handle.await.unwrap().is_none());

                const SERVER_DECLINE: &str = "docs-url-elicit-decline";
                let decline = rmcp::model::ElicitRequestParams::UrlElicitationParams {
                    meta: None,
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: "elicit-decline".to_string(),
                };
                let decline_handle = tokio::spawn(runtime::process_url_elicitation_required(
                    SERVER_DECLINE,
                    "login",
                    vec![decline],
                    crate::tool::HandleElicitationCallback::default(),
                ));
                tokio::task::yield_now().await;
                let events = runtime::drain_mcp_connection_callback_observations().await;
                let request_id = match events.as_slice() {
                    [crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ElicitationRequested { event }] => event.request_id.clone(),
                    other => panic!("expected one URL elicitation event, got {other:?}"),
                };
                assert!(
                    runtime::respond_to_mcp_elicitation(
                        SERVER_DECLINE,
                        &request_id,
                        ElicitationResult::new(ElicitationAction::Decline),
                    )
                    .await
                );
                let result = decline_handle.await.unwrap().expect("decline result");
                assert_eq!(
                    result["content"][0]["text"].as_str(),
                    Some(
                        url_elicitation_required_result_message(
                            ElicitationAction::Decline,
                            "the user",
                            "login",
                        )
                        .as_str()
                    )
                );

                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_url_elicitation_required_retries_after_user_accepts() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-url-elicit-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
let callCount = 0
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
        capabilities: { tools: {} },
        serverInfo: { name: 'url-elicit-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        tools: [{
          name: 'login',
          description: 'Login after opening a URL',
          inputSchema: { type: 'object', properties: {}, additionalProperties: false }
        }]
      }
    })
  } else if (message.method === 'tools/call') {
    callCount += 1
    if (callCount === 1) {
      send({
        jsonrpc: '2.0',
        id: message.id,
        error: {
          code: -32042,
          message: 'URL elicitation required',
          data: {
            elicitations: [{
              mode: 'url',
              url: 'https://example.com/auth',
              elicitationId: 'fixture-elicit',
              message: 'Authorize this MCP tool'
            }]
          }
        }
      })
    } else {
      send({
        jsonrpc: '2.0',
        id: message.id,
        result: { content: [{ type: 'text', text: 'retried after url' }] }
      })
    }
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "url-elicit-stdio-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.tools.len(), 1);
                assert_eq!(discovery.server.tools[0].name, "login");

                let call = tokio::spawn(async {
                    runtime::call_mcp_tool(SERVER, "login", Map::new()).await
                });
                let request_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        for event in runtime::drain_mcp_connection_callback_observations().await {
                            if let crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ElicitationRequested { event } = event {
                                assert_eq!(event.server_name, SERVER);
                                assert_eq!(event.request_id, "error-elicit-fixture-elicit");
                                assert!(event.waiting_state.is_some());
                                return event.request_id;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("URL elicitation event should be queued");

                assert!(
                    runtime::respond_to_mcp_elicitation(
                        SERVER,
                        &request_id,
                        ElicitationResult::new(ElicitationAction::Accept),
                    )
                    .await
                );
                let value = call.await.unwrap().expect("tool call should retry");
                assert_eq!(
                    value["content"][0]["text"].as_str(),
                    Some("retried after url")
                );
                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_tools_list_changed_notification_refreshes_live_tools() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-tools-list-changed-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
let listCount = 0
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
function tool(name) {
  return {
    name,
    description: `Tool ${name}`,
    inputSchema: { type: 'object', properties: {}, additionalProperties: false }
  }
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
        capabilities: { tools: { listChanged: true } },
        serverInfo: { name: 'tools-list-changed-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    listCount += 1
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { tools: [tool(listCount === 1 ? 'first' : 'second')] }
    })
    if (listCount === 1) {
      setTimeout(() => send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' }), 10)
    }
  } else if (message.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: 'ok' }] } })
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "tools-list-changed-stdio-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.tools[0].name, "first");

                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        for event in runtime::drain_mcp_connection_callback_observations().await {
                            if let crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ToolsListChanged { name } = event {
                                assert_eq!(name, SERVER);
                                return;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("tools/list_changed should be emitted");

                let refreshed = runtime::refresh_mcp_tools_for_client(SERVER)
                    .await
                    .expect("tools refresh should use the connected peer");
                assert_eq!(refreshed.len(), 1);
                assert_eq!(refreshed[0].name, "second");
                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_prompts_and_resources_list_changed_refresh_live_state() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-prompts-resources-list-changed-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
let promptListCount = 0
let resourceListCount = 0
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
function prompt(name) {
  return {
    name,
    description: `Prompt ${name}`,
    arguments: [{ name: 'topic', description: 'Topic', required: false }]
  }
}
function resource(name) {
  return {
    uri: `file:///tmp/${name}.txt`,
    name,
    description: `Resource ${name}`,
    mimeType: 'text/plain'
  }
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
        capabilities: {
          tools: {},
          prompts: { listChanged: true },
          resources: { listChanged: true }
        },
        serverInfo: { name: 'prompts-resources-list-changed-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: message.id, result: { tools: [] } })
  } else if (message.method === 'prompts/list') {
    promptListCount += 1
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { prompts: [prompt(promptListCount === 1 ? 'first_prompt' : 'second_prompt')] }
    })
    if (promptListCount === 1) {
      setTimeout(() => send({ jsonrpc: '2.0', method: 'notifications/prompts/list_changed' }), 10)
    }
  } else if (message.method === 'resources/list') {
    resourceListCount += 1
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { resources: [resource(resourceListCount === 1 ? 'first_resource' : 'second_resource')] }
    })
    if (resourceListCount === 1) {
      setTimeout(() => send({ jsonrpc: '2.0', method: 'notifications/resources/list_changed' }), 10)
    }
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "prompts-resources-list-changed-stdio-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.prompts[0].name, "first_prompt");
                assert_eq!(discovery.server.resources[0].name, "first_resource");

                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    let mut saw_prompts = false;
                    let mut saw_resources = false;
                    loop {
                        for event in runtime::drain_mcp_connection_callback_observations().await {
                            match event {
                                crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::PromptsListChanged { name } => {
                                    assert_eq!(name, SERVER);
                                    saw_prompts = true;
                                }
                                crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ResourcesListChanged { name } => {
                                    assert_eq!(name, SERVER);
                                    saw_resources = true;
                                }
                                _ => {}
                            }
                        }
                        if saw_prompts && saw_resources {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("prompts/resources list_changed should be emitted");

                let refreshed_prompts = runtime::refresh_mcp_prompts_for_client(SERVER)
                    .await
                    .expect("prompts refresh should use the connected peer");
                assert_eq!(refreshed_prompts.len(), 1);
                assert_eq!(refreshed_prompts[0].name, "second_prompt");
                assert_eq!(refreshed_prompts[0].arg_names, vec!["topic"]);

                let refreshed_resources = runtime::refresh_mcp_resources_for_client(SERVER)
                    .await
                    .expect("resources refresh should use the connected peer");
                assert_eq!(refreshed_resources.len(), 1);
                assert_eq!(refreshed_resources[0].name, "second_resource");
                assert_eq!(
                    refreshed_resources[0].uri,
                    "file:///tmp/second_resource.txt"
                );
                assert_eq!(
                    refreshed_resources[0].mime_type.as_deref(),
                    Some("text/plain")
                );

                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_read_mcp_resource_reads_text_content_from_live_server() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-read-resource-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
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
        serverInfo: { name: 'read-resource-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'resources/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        resources: [{
          uri: 'file:///tmp/cometix-resource.txt',
          name: 'cometix-resource',
          description: 'A text resource',
          mimeType: 'text/plain'
        }]
      }
    })
  } else if (message.method === 'resources/read') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        contents: [{
          uri: message.params.uri,
          mimeType: 'text/plain',
          text: 'hello from read_resource'
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "read-resource-stdio-fixture";
                let uri = "file:///tmp/cometix-resource.txt";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.resources.len(), 1);
                assert_eq!(discovery.server.resources[0].uri, uri);

                let value = runtime::read_mcp_resource(SERVER, uri)
                    .await
                    .expect("resource read should succeed");
                assert_eq!(value["contents"][0]["uri"].as_str(), Some(uri));
                assert_eq!(
                    value["contents"][0]["mimeType"].as_str(),
                    Some("text/plain")
                );
                assert_eq!(
                    value["contents"][0]["text"].as_str(),
                    Some("hello from read_resource")
                );
                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_read_mcp_resource_checks_resource_capability_like_official_tool() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-read-resource-no-capability-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
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
        capabilities: { tools: {} },
        serverInfo: { name: 'no-resource-capability-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: message.id, result: { tools: [] } })
  } else if (message.method === 'resources/read') {
    send({ jsonrpc: '2.0', id: message.id, result: { contents: [{ uri: message.params.uri, text: 'should not be read' }] } })
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "read-resource-no-capability-stdio-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert!(discovery.server.resources.is_empty());

                let error = runtime::read_mcp_resource(SERVER, "file:///tmp/missing.txt")
                    .await
                    .expect_err("read should be rejected before resources/read");
                assert_eq!(
                    error.to_string(),
                    "Server \"read-resource-no-capability-stdio-fixture\" does not support resources"
                );
                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn stdio_get_mcp_prompt_for_command_fetches_live_prompt_content() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-mcp-get-prompt-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
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
        capabilities: { prompts: {} },
        serverInfo: { name: 'get-prompt-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'prompts/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        prompts: [{
          name: 'daily',
          description: 'Build a daily summary prompt',
          arguments: [{ name: 'topic', description: 'Topic', required: false }]
        }]
      }
    })
  } else if (message.method === 'prompts/get') {
    const topic = message.params.arguments?.topic ?? ''
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        description: 'Daily summary',
        messages: [{
          role: 'user',
          content: { type: 'text', text: `summarize:${topic}` }
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
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "get-prompt-stdio-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
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
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.prompts.len(), 1);
                assert_eq!(discovery.server.prompts[0].name, "daily");
                assert_eq!(discovery.server.prompts[0].arg_names, vec!["topic"]);

                let blocks = runtime::get_mcp_prompt_for_command(
                    SERVER,
                    "daily",
                    &["topic".to_string()],
                    "roadmap",
                )
                .await
                .expect("prompt get should succeed");
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0]["type"].as_str(), Some("text"));
                assert_eq!(blocks[0]["text"].as_str(), Some("summarize:roadmap"));
                runtime::clear_server_cache(SERVER, None).await;
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn streamable_http_connects_and_discovers_tools_from_live_server() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::TcpListener;

                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind streamable HTTP fixture");
                let addr = listener.local_addr().expect("fixture address");
                let server = tokio::spawn(async move {
                    let mut tools_list_seen = false;
                    let mut requests = Vec::<String>::new();
                    for _ in 0..6 {
                        let (mut socket, _) = listener.accept().await.expect("accept fixture");
                        let mut buffer = Vec::new();
                        let mut temp = [0_u8; 1024];
                        let mut content_length = None::<usize>;
                        loop {
                            let read = socket.read(&mut temp).await.expect("read request");
                            if read == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&temp[..read]);
                            let raw = String::from_utf8_lossy(&buffer);
                            if let Some(header_end) = raw.find("\r\n\r\n") {
                                if content_length.is_none() {
                                    content_length = raw[..header_end].lines().find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    });
                                }
                                let needed = header_end + 4 + content_length.unwrap_or(0);
                                if buffer.len() >= needed {
                                    break;
                                }
                            }
                        }
                        let raw = String::from_utf8_lossy(&buffer).to_string();
                        requests.push(raw.clone());
                        let body = raw
                            .split("\r\n\r\n")
                            .nth(1)
                            .unwrap_or_default()
                            .to_string();
                        let message: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({}));
                        let method = message
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let id = message.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let response = match method {
                            "initialize" => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "protocolVersion": "2025-11-25",
                                    "capabilities": { "tools": {} },
                                    "serverInfo": { "name": "streamable-http-fixture", "version": "1.0.0" }
                                }
                            }),
                            "notifications/initialized" => {
                                socket
                                    .write_all(
                                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                    )
                                    .await
                                    .expect("write initialized response");
                                continue;
                            }
                            "tools/list" => {
                                tools_list_seen = true;
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "tools": [{
                                            "name": "lookup",
                                            "description": "Lookup docs over HTTP MCP",
                                            "inputSchema": {
                                                "type": "object",
                                                "properties": { "query": { "type": "string" } },
                                                "additionalProperties": false
                                            }
                                        }]
                                    }
                                })
                            }
                            other => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32601, "message": format!("method not found: {other}") }
                            }),
                        };
                        let response_body = response.to_string();
                        let wire = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response_body.len(),
                            response_body
                        );
                        socket
                            .write_all(wire.as_bytes())
                            .await
                            .expect("write response");
                        if tools_list_seen {
                            break;
                        }
                    }
                    assert!(tools_list_seen, "requests={requests:#?}");
                    requests
                });

                const SERVER: &str = "streamable-http-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Http,
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: Some(format!("http://{addr}/mcp")),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.client.server_version.as_deref(), Some("1.0.0"));
                assert_eq!(discovery.server.tools.len(), 1);
                assert_eq!(discovery.server.tools[0].name, "lookup");
                assert_eq!(
                    discovery.server.tools[0].description.as_deref(),
                    Some("Lookup docs over HTTP MCP")
                );
                assert_eq!(discovery.server.config.as_ref().unwrap().transport, Transport::Http);
                runtime::clear_server_cache(SERVER, None).await;
                let _requests = server.await.expect("fixture server task");
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn legacy_sse_connects_and_discovers_tools_from_live_server() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::{TcpListener, TcpStream};
                use tokio::sync::mpsc;

                async fn read_http_request(socket: &mut TcpStream) -> String {
                    let mut buffer = Vec::new();
                    let mut temp = [0_u8; 1024];
                    let mut content_length = None::<usize>;
                    loop {
                        let read = socket.read(&mut temp).await.expect("read request");
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&temp[..read]);
                        let raw = String::from_utf8_lossy(&buffer);
                        if let Some(header_end) = raw.find("\r\n\r\n") {
                            if content_length.is_none() {
                                content_length = raw[..header_end].lines().find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                });
                            }
                            let needed = header_end + 4 + content_length.unwrap_or(0);
                            if buffer.len() >= needed {
                                break;
                            }
                        }
                    }
                    String::from_utf8_lossy(&buffer).to_string()
                }

                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind legacy SSE fixture");
                let addr = listener.local_addr().expect("fixture address");
                let server = tokio::spawn(async move {
                    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                    let (mut sse_socket, _) = listener.accept().await.expect("accept SSE GET");
                    let get_request = read_http_request(&mut sse_socket).await;
                    assert!(get_request.starts_with("GET /sse HTTP/1.1"), "{get_request}");
                    assert!(
                        get_request.to_ascii_lowercase().contains("accept: text/event-stream"),
                        "{get_request}"
                    );
                    sse_socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\nevent: endpoint\ndata: /messages\r\n\r\n",
                        )
                        .await
                        .expect("write SSE endpoint event");

                    let writer = tokio::spawn(async move {
                        while let Some(payload) = rx.recv().await {
                            let event = format!("event: message\ndata: {payload}\r\n\r\n");
                            if sse_socket.write_all(event.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                    });

                    let mut requests = Vec::<String>::new();
                    let mut tools_list_seen = false;
                    for _ in 0..4 {
                        let (mut socket, _) = listener.accept().await.expect("accept POST");
                        let raw = read_http_request(&mut socket).await;
                        assert!(raw.starts_with("POST /messages HTTP/1.1"), "{raw}");
                        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
                        let message: serde_json::Value =
                            serde_json::from_str(body).expect("MCP JSON-RPC POST body");
                        requests.push(raw);
                        let method = message
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let id = message.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        match method {
                            "initialize" => {
                                tx.send(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {
                                            "protocolVersion": "2025-11-25",
                                            "capabilities": { "tools": {} },
                                            "serverInfo": { "name": "legacy-sse-fixture", "version": "1.0.0" }
                                        }
                                    })
                                    .to_string(),
                                )
                                .expect("queue initialize response");
                            }
                            "notifications/initialized" => {}
                            "tools/list" => {
                                tools_list_seen = true;
                                tx.send(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {
                                            "tools": [{
                                                "name": "search",
                                                "description": "Search docs over SSE MCP",
                                                "inputSchema": {
                                                    "type": "object",
                                                    "properties": { "query": { "type": "string" } },
                                                    "additionalProperties": false
                                                }
                                            }]
                                        }
                                    })
                                    .to_string(),
                                )
                                .expect("queue tools/list response");
                            }
                            other => {
                                tx.send(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "error": { "code": -32601, "message": format!("method not found: {other}") }
                                    })
                                    .to_string(),
                                )
                                .expect("queue error response");
                            }
                        }
                        socket
                            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .expect("write POST response");
                        if tools_list_seen {
                            break;
                        }
                    }
                    assert!(tools_list_seen, "requests={requests:#?}");
                    drop(tx);
                    let _ = writer.await;
                    requests
                });

                const SERVER: &str = "legacy-sse-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Sse,
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: Some(format!("http://{addr}/sse")),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.client.server_version.as_deref(), Some("1.0.0"));
                assert_eq!(discovery.server.tools.len(), 1);
                assert_eq!(discovery.server.tools[0].name, "search");
                assert_eq!(
                    discovery.server.tools[0].description.as_deref(),
                    Some("Search docs over SSE MCP")
                );
                assert_eq!(discovery.server.config.as_ref().unwrap().transport, Transport::Sse);
                runtime::clear_server_cache(SERVER, None).await;
                let _requests = server.await.expect("fixture server task");
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn websocket_connects_and_discovers_tools_from_live_server() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use futures::{SinkExt, StreamExt};
                use tokio::net::TcpListener;
                use tokio_tungstenite::accept_hdr_async;
                use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
                use tokio_tungstenite::tungstenite::protocol::Message;

                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind WebSocket fixture");
                let addr = listener.local_addr().expect("fixture address");
                let server = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.expect("accept WebSocket");
                    let mut websocket = accept_hdr_async(
                        stream,
                        |request: &Request, mut response: Response| {
                            assert_eq!(request.uri().path(), "/mcp");
                            assert!(
                                request
                                    .headers()
                                    .get("sec-websocket-protocol")
                                    .and_then(|value| value.to_str().ok())
                                    .is_some_and(|value| value.split(',').any(|part| part.trim() == "mcp")),
                                "request={request:?}"
                            );
                            response.headers_mut().insert(
                                "Sec-WebSocket-Protocol",
                                http::HeaderValue::from_static("mcp"),
                            );
                            Ok(response)
                        },
                    )
                    .await
                    .expect("accept WebSocket handshake");

                    let mut initialized_seen = false;
                    let mut tools_list_seen = false;
                    while let Some(message) = websocket.next().await {
                        let message = message.expect("websocket message");
                        let Message::Text(payload) = message else {
                            continue;
                        };
                        let message: serde_json::Value =
                            serde_json::from_str(&payload).expect("MCP JSON-RPC websocket body");
                        let method = message
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let id = message.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        match method {
                            "initialize" => {
                                websocket
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "result": {
                                                "protocolVersion": "2025-11-25",
                                                "capabilities": { "tools": {} },
                                                "serverInfo": { "name": "websocket-fixture", "version": "1.0.0" }
                                            }
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await
                                    .expect("send initialize response");
                            }
                            "notifications/initialized" => {
                                initialized_seen = true;
                            }
                            "tools/list" => {
                                tools_list_seen = true;
                                websocket
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "result": {
                                                "tools": [{
                                                    "name": "inspect",
                                                    "description": "Inspect docs over WebSocket MCP",
                                                    "inputSchema": {
                                                        "type": "object",
                                                        "properties": { "query": { "type": "string" } },
                                                        "additionalProperties": false
                                                    }
                                                }]
                                            }
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await
                                    .expect("send tools/list response");
                                break;
                            }
                            other => {
                                websocket
                                    .send(Message::Text(
                                        serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "error": { "code": -32601, "message": format!("method not found: {other}") }
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await
                                    .expect("send error response");
                            }
                        }
                    }
                    assert!(initialized_seen);
                    assert!(tools_list_seen);
                    let _ = websocket.close(None).await;
                });

                const SERVER: &str = "websocket-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Ws,
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: Some(format!("ws://{addr}/mcp")),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.client.server_version.as_deref(), Some("1.0.0"));
                assert_eq!(discovery.server.tools.len(), 1);
                assert_eq!(discovery.server.tools[0].name, "inspect");
                assert_eq!(
                    discovery.server.tools[0].description.as_deref(),
                    Some("Inspect docs over WebSocket MCP")
                );
                assert_eq!(discovery.server.config.as_ref().unwrap().transport, Transport::Ws);
                runtime::clear_server_cache(SERVER, None).await;
                server.await.expect("fixture server task");
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn streamable_http_401_challenge_returns_unavailable_without_credential_or_auth_cache_write() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("cometix-mcp-http-401-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).expect("create config home");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::TcpListener;

                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind streamable HTTP auth fixture");
                let addr = listener.local_addr().expect("fixture address");
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.expect("accept initialize POST");
                    let mut buffer = Vec::new();
                    let mut temp = [0_u8; 1024];
                    let mut content_length = None::<usize>;
                    loop {
                        let read = socket.read(&mut temp).await.expect("read request");
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&temp[..read]);
                        let raw = String::from_utf8_lossy(&buffer);
                        if let Some(header_end) = raw.find("\r\n\r\n") {
                            if content_length.is_none() {
                                content_length = raw[..header_end].lines().find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                });
                            }
                            let needed = header_end + 4 + content_length.unwrap_or(0);
                            if buffer.len() >= needed {
                                break;
                            }
                        }
                    }
                    let raw = String::from_utf8_lossy(&buffer).to_string();
                    assert!(raw.starts_with("POST /mcp HTTP/1.1"), "{raw}");
                    assert!(raw.contains("initialize"), "{raw}");
                    socket
                        .write_all(
                            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"admin write\", resource_metadata=\"/.well-known/oauth-protected-resource/mcp\"\r\nContent-Length: 12\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nunauthorized",
                        )
                        .await
                        .expect("write auth challenge");
                });

                const SERVER: &str = "http-auth-fixture";
                let server_url = format!("http://{addr}/mcp");
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Http,
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: Some(server_url.clone()),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    oauth: Some(serde_json::json!({"clientId":"client"})),
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(discovery.server.client.status, McpServerConnectionType::Failed);
                assert_eq!(
                    discovery.server.client.error.as_deref(),
                    Some(
                        crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
                    )
                );
                assert!(discovery.server.tools.is_empty());
                server.await.expect("fixture server task");
                let _ = runtime::drain_mcp_connection_callback_observations().await;
                assert!(!config_home.join(".credentials.json").exists());
                assert!(!config_home.join("mcp-needs-auth-cache.json").exists());
            });

        let _ = std::fs::remove_dir_all(config_home);
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn streamable_http_call_mcp_tool_uses_live_peer_and_meta() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::TcpListener;

                async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
                    let mut buffer = Vec::new();
                    let mut temp = [0_u8; 1024];
                    let mut content_length = None::<usize>;
                    loop {
                        let read = socket.read(&mut temp).await.expect("read request");
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&temp[..read]);
                        let raw = String::from_utf8_lossy(&buffer);
                        if let Some(header_end) = raw.find("\r\n\r\n") {
                            if content_length.is_none() {
                                content_length = raw[..header_end].lines().find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                });
                            }
                            let needed = header_end + 4 + content_length.unwrap_or(0);
                            if buffer.len() >= needed {
                                break;
                            }
                        }
                    }
                    String::from_utf8_lossy(&buffer).to_string()
                }

                let _ = runtime::drain_mcp_connection_callback_observations().await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind streamable HTTP call fixture");
                let addr = listener.local_addr().expect("fixture address");
                let server = tokio::spawn(async move {
                    let mut tool_call_seen = false;
                    for _ in 0..8 {
                        let (mut socket, _) = listener.accept().await.expect("accept request");
                        let raw = read_http_request(&mut socket).await;
                        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
                        let message: serde_json::Value =
                            serde_json::from_str(body).unwrap_or_else(|_| serde_json::json!({}));
                        let method = message
                            .get("method")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let id = message.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let response = match method {
                            "initialize" => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "protocolVersion": "2025-11-25",
                                    "capabilities": { "tools": {} },
                                    "serverInfo": { "name": "streamable-http-call-fixture", "version": "1.0.0" }
                                }
                            }),
                            "notifications/initialized" => {
                                socket
                                    .write_all(
                                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                    )
                                    .await
                                    .expect("write initialized response");
                                continue;
                            }
                            "tools/list" => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "tools": [{
                                        "name": "lookup",
                                        "description": "Lookup docs over HTTP MCP",
                                        "inputSchema": {
                                            "type": "object",
                                            "properties": { "query": { "type": "string" } },
                                            "additionalProperties": false
                                        }
                                    }]
                                }
                            }),
                            "tools/call" => {
                                tool_call_seen = true;
                                assert_eq!(
                                    message["params"]["name"].as_str(),
                                    Some("lookup")
                                );
                                assert_eq!(
                                    message["params"]["arguments"]["query"].as_str(),
                                    Some("manual")
                                );
                                assert_eq!(
                                    message["params"]["_meta"]["claudecode/toolUseId"].as_str(),
                                    Some("toolu_http")
                                );
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "content": [{ "type": "text", "text": "lookup:manual:toolu_http" }]
                                    }
                                })
                            }
                            other => serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32601, "message": format!("method not found: {other}") }
                            }),
                        };
                        let response_body = response.to_string();
                        let wire = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response_body.len(),
                            response_body
                        );
                        socket
                            .write_all(wire.as_bytes())
                            .await
                            .expect("write response");
                        if tool_call_seen {
                            break;
                        }
                    }
                    assert!(tool_call_seen);
                });

                const SERVER: &str = "streamable-http-call-fixture";
                let config = ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: Transport::Http,
                    command: None,
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    url: Some(format!("http://{addr}/mcp")),
                    headers: BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.tools.len(), 1);
                let value = runtime::call_mcp_tool_with_meta(
                    SERVER,
                    "lookup",
                    Map::from_iter([("query".to_string(), Value::String("manual".to_string()))]),
                    Some(mcp_tool_use_id_meta("toolu_http")),
                )
                .await
                .expect("HTTP MCP tool call should succeed");
                assert_eq!(
                    value["content"][0]["text"].as_str(),
                    Some("lookup:manual:toolu_http")
                );
                runtime::clear_server_cache(SERVER, None).await;
                server.await.expect("fixture server task");
                let _ = runtime::drain_mcp_connection_callback_observations().await;
            });
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn streamable_http_auth_header_precedence_matches_official_connect_to_server() {
        assert_eq!(
            runtime::streamable_http_auth_header(
                Some("oauth-token".to_string()),
                Some("session-token".to_string()),
                false,
            )
            .as_deref(),
            Some("oauth-token")
        );
        assert_eq!(
            runtime::streamable_http_auth_header(None, Some("session-token".to_string()), false,)
                .as_deref(),
            Some("session-token")
        );
        assert_eq!(
            runtime::streamable_http_auth_header(None, Some("session-token".to_string()), true,),
            None
        );
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn runtime_auth_helpers_extract_legacy_sse_www_authenticate_challenges() {
        let insufficient_scope_header =
            "Bearer error=\"insufficient_scope\", scope=\"admin write\"".to_string();
        let insufficient_scope = anyhow::Error::new(runtime::McpRemoteAuthHttpError {
            status: 403,
            www_authenticate: Some(insufficient_scope_header.clone()),
            body: "forbidden".to_string(),
        });
        assert!(runtime::is_auth_error(&insufficient_scope));
        assert!(runtime::is_insufficient_scope_auth_error(
            &insufficient_scope
        ));
        assert_eq!(
            runtime::streamable_http_challenge_from_anyhow(&insufficient_scope),
            Some((insufficient_scope_header, Some("admin write".to_string())))
        );

        let unauthorized = anyhow::Error::new(runtime::McpRemoteAuthHttpError {
            status: 401,
            www_authenticate: Some("Bearer realm=\"mcp\"".to_string()),
            body: "unauthorized".to_string(),
        });
        assert!(runtime::is_auth_error(&unauthorized));
        assert!(!runtime::is_insufficient_scope_auth_error(&unauthorized));
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn cached_needs_auth_skips_remote_probe_and_projects_auth_tool() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("cometix-mcp-auth-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &temp_dir);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        std::fs::write(
            temp_dir.join("mcp-needs-auth-cache.json"),
            serde_json::json!({"docs":{"timestamp":now_ms}}).to_string(),
        )
        .unwrap();

        let config = ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::User,
            transport: Transport::Http,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("http://127.0.0.1:9/mcp".to_string()),
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: Some(serde_json::json!({"clientId":"client"})),
            ide_running_in_windows: None,
            ide_name: None,
            auth_token: None,
            id: None,
            plugin_source: None,
        };
        let discoveries = std::sync::Mutex::new(Vec::new());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(get_mcp_tools_commands_and_resources(
                |discovery| discoveries.lock().unwrap().push(discovery),
                &indexmap::IndexMap::from([("docs".to_string(), config)]),
            ));
        let discoveries = discoveries.into_inner().unwrap();

        let _ = std::fs::remove_dir_all(&temp_dir);

        assert_eq!(discoveries.len(), 1);
        assert_eq!(
            discoveries[0].server.client.status,
            McpServerConnectionType::NeedsAuth
        );
        assert_eq!(discoveries[0].server.tools[0].name, "authenticate");
    }

    #[test]
    fn mcp_tool_description_matches_official_untruncated_description_callback() {
        // CC client.ts:1786-1794: description preserves the entire original
        // text, whereas prompt separately applies MAX_MCP_DESCRIPTION_LENGTH.
        let raw = "metadata ".repeat(MAX_MCP_DESCRIPTION_LENGTH + 1);
        let mut tool = crate::services::mcp::types::McpToolSnapshot {
            name: "read".into(),
            display_name: None,
            description: Some(raw.clone()),
            input_schema: serde_json::json!({"type":"object"}),
            read_only_hint: true,
            destructive_hint: false,
            open_world_hint: false,
        };
        assert_eq!(mcp_tool_description(&tool), raw);
        assert_ne!(truncate_mcp_description(tool.description.clone()), raw);
        tool.description = None;
        assert_eq!(mcp_tool_description(&tool), "");
    }
}

/// Test fixture for callback-built state. Production must retain the actual
/// connection callback arrays instead of reconstructing them from snapshots.
#[cfg(test)]
pub fn refresh_flat_mcp_capabilities(state: &mut McpState) {
    let added = std::sync::atomic::AtomicBool::new(false);
    state.tools.clear();
    state.commands.clear();
    state.resources.clear();
    for server in &state.clients {
        let mut discovery = McpConnectionDiscovery::from(server.clone());
        discovery.add_discovery_resource_tools(&added);
        state.tools.extend(discovery.tools);
        state.commands.extend(discovery.commands);
        if let Some(resources) = discovery.resources {
            state
                .resources
                .insert(server.client.name.clone(), resources);
        }
    }
}
