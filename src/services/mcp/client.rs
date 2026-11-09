//! MCP client layer.
//! Maps to: CC `services/mcp/client.ts`.
//!
//! Claude Code uses `@modelcontextprotocol/sdk`'s `Client`,
//! `StdioClientTransport`, and `StreamableHTTPClientTransport`. Nyan maps
//! those responsibilities to the official Rust SDK for MCP
//! (`modelcontextprotocol/rust-sdk`, crate `rmcp`). This module owns transport
//! startup, client connection, tool/prompt/resource discovery, reconnect cache
//! invalidation, and MCP tool calls. Session callbacks
//! (`onclose` / notification handlers) are owned by
//! [`crate::services::mcp::use_manage_mcp_connections`] (CC `onConnectionAttempt`). UI
//! components and slash commands must not instantiate transports directly.

use super::elicitation_handler::{
    ElicitationAction, ElicitationMode, ElicitationRequestEvent, ElicitationResult,
    ElicitationWaitingState, PENDING_ELICITATION_RESPONSES, elicitation_request_json,
    rmcp_elicitation_params_to_event_params, run_elicitation_hooks, run_elicitation_result_hooks,
};
use super::types::{
    ConnectedMcpServer, McpServerConnection, McpServerConnectionType, ScopedMcpServerConfig,
    ServerResource, Transport,
};
use crate::state::app_state_store::McpState;
// CC's `String(...)` / template-literal conversion on MCP payload fields
// (`client.ts:2505` `String(resultContent.data)`, `:2670`
// `String(result.toolResult)`, and the `${resource.uri}` / `${resource.text}` /
// `${resourceLink.name}` templates inside `transformResultContent`). The
// conversion belongs to the zod carrier's `z.coerce.string()` port, which is the
// only place `Number::toString` (`1`, not `1.0`), `[object Object]`, and the
// array comma-join are implemented.
use crate::utils::zod::js_string;
use serde_json::{Map, Value};

/// Test observation of the session-level MCP events
/// ([`crate::services::mcp::use_manage_mcp_connections::OnConnectionAttemptHandlers`],
/// which App registers): a tool call's URL elicitation queue push.
pub use crate::services::mcp::use_manage_mcp_connections::drain_mcp_connection_callback_observations;

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
        crate::utils::process_env::var("ENABLE_MCP_LARGE_OUTPUT_FILES").as_deref(),
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

/// Maps to: CC `isIncludedMcpTool` (`client.ts:569-573`), which
/// `fetchToolsForClient` applies to each finished tool (`:1990`), so to its
/// final name.
fn is_included_mcp_tool(tool: &crate::types::tools::Tool) -> bool {
    const ALLOWED_IDE_TOOLS: &[&str] = &["mcp__ide__executeCode", "mcp__ide__getDiagnostics"];
    !tool.name.starts_with("mcp__ide__") || ALLOWED_IDE_TOOLS.contains(&tool.name.as_str())
}

/// Maps to: CC `fetchToolsForClient`'s `skipPrefix` (`client.ts:1761-1763`):
/// an SDK server's tools keep their own names under
/// `NYAN_SDK_MCP_NO_PREFIX`.
fn skips_mcp_tool_prefix(config: &ScopedMcpServerConfig) -> bool {
    config.transport == Transport::Sdk
        && crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("NYAN_SDK_MCP_NO_PREFIX").as_deref(),
        )
}

/// Maps to: CC `fetchToolsForClient`'s per-tool map (`client.ts:1767-1988`):
/// the server's tool as an `mcp.tools` entry, bound to the `client` it was
/// fetched from. Resource helper tools are added by the discovery and
/// reconnect callers, not here.
fn mcp_tool_from_server_tool(
    client: &std::sync::Arc<ConnectedMcpServer>,
    skip_prefix: bool,
    tool: rmcp::model::Tool,
) -> crate::types::tools::Tool {
    let server_name = client.name.as_str();
    let annotations = tool.annotations.unwrap_or_default();
    let tool_name = tool.name.to_string();
    let description = tool.description.map(|description| description.to_string());
    crate::types::tools::Tool {
        name: if skip_prefix {
            tool_name.clone()
        } else {
            crate::services::mcp::mcp_string_utils::build_mcp_tool_name(server_name, &tool_name)
        },
        description: truncate_mcp_description(description.clone()),
        input_schema: Value::Object((*tool.input_schema).clone()),
        is_mcp: true,
        // Maps to: CC `client.ts:1774` — populated for both prefixed and
        // skip-prefix tools, so permission rule matching can rebuild the
        // qualified name either way.
        mcp_info: Some(crate::types::tools::McpToolInfo {
            server_name: server_name.to_string(),
            tool_name,
        }),
        // CC reads `tool.annotations?.title` only (`client.ts:1974`), not the
        // tool's top-level `title`.
        mcp_annotations: crate::types::tools::McpToolAnnotations {
            title: annotations.title,
            read_only_hint: annotations.read_only_hint.unwrap_or(false),
            destructive_hint: annotations.destructive_hint.unwrap_or(false),
            open_world_hint: annotations.open_world_hint.unwrap_or(false),
        },
        mcp_description: description.unwrap_or_default(),
        mcp_connection: Some(crate::types::tools::McpToolConnection::Connected(
            client.clone(),
        )),
        ..Default::default()
    }
}

/// Maps to: CC `fetchToolsForClient`'s result (`client.ts:1766-1990`): one
/// `tools/list`'s tools, mapped and filtered by `isIncludedMcpTool`.
fn mcp_tools_from_server_tools(
    client: &std::sync::Arc<ConnectedMcpServer>,
    tools: Vec<rmcp::model::Tool>,
) -> Vec<crate::types::tools::Tool> {
    let skip_prefix = skips_mcp_tool_prefix(&client.config);
    tools
        .into_iter()
        .map(|tool| mcp_tool_from_server_tool(client, skip_prefix, tool))
        .filter(is_included_mcp_tool)
        .collect()
}

/// A server tool as `fetchToolsForClient` makes it, from the `tools/list`
/// entry's JSON, for tests that need an `mcp.tools` entry. It is bound to a
/// connected record with no connection behind it.
#[cfg(test)]
pub(crate) fn mcp_tool_for_test(server_name: &str, tool: Value) -> crate::types::tools::Tool {
    mcp_tool_from_server_tool(
        &connected_record_for_test(server_name),
        false,
        serde_json::from_value(tool).expect("an MCP tool"),
    )
}

/// A connected record with no connection behind it, as a tool or command
/// binding.
#[cfg(test)]
fn connected_record_for_test(server_name: &str) -> std::sync::Arc<ConnectedMcpServer> {
    match McpServerConnection::connected_for_test(
        server_name,
        ScopedMcpServerConfig::stdio_for_test(server_name),
    ) {
        McpServerConnection::Connected(connected) => std::sync::Arc::new(connected),
        _ => unreachable!("connected_for_test makes a connected record"),
    }
}

/// Maps to: CC `options.hasPendingMCPServers` request option.
pub fn has_pending_mcp_servers(state: &McpState) -> bool {
    state
        .clients
        .iter()
        .any(|server| server.connection_type() == McpServerConnectionType::Pending)
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

/// Maps to: CC `services/mcp/client.ts:1972-1976` — the `userFacingName()` that
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

/// Maps to: CC `client.ts:1973` `tool.annotations?.title || tool.name` — the
/// `||` is JS truthiness, so an empty title falls through to the tool name.
pub fn mcp_tool_display_name(annotation_title: Option<&str>, tool_name: &str) -> String {
    annotation_title
        .filter(|title| !title.is_empty())
        .unwrap_or(tool_name)
        .to_string()
}

/// Maps to: CC `fetchCommandsForClient(...)` `zipObject(argNames,
/// args.split(' '))` in `getPromptForCommand(args)`: a name without a word
/// gets `undefined`, which the request's JSON leaves out.
pub fn mcp_prompt_arguments_from_args(arg_names: &[String], args: &str) -> Map<String, Value> {
    arg_names
        .iter()
        .zip(args.split(' '))
        .map(|(name, arg)| (name.clone(), Value::String(arg.to_string())))
        .collect()
}

/// A server prompt as `fetchCommandsForClient` makes it, for tests that need
/// an `mcp.commands` entry. It is bound to a connected record with no
/// connection behind it.
#[cfg(test)]
pub(crate) fn mcp_command_for_test(
    server_name: &str,
    prompt_name: &str,
    description: Option<&str>,
    arg_names: &[&str],
) -> crate::commands::Command {
    crate::commands::Command::from_mcp_prompt(
        &connected_record_for_test(server_name),
        prompt_name,
        description,
        arg_names.iter().map(|name| name.to_string()).collect(),
    )
}

/// Maps to: CC `findCommand(commandName, commands)` (`commands.ts:688-698`)
/// over the REPL's merged commands, when what it finds is a prompt command
/// the MCP fetch made. Like any command it matches by name, user-facing
/// name (`server:prompt (MCP)`) or alias. It carries the client it was
/// fetched from, whose `getPromptForCommand` goes through
/// `ensureConnectedClient` whatever that server's record says now.
pub fn find_mcp_prompt_command<'a>(
    command_name: &str,
    commands: &'a [crate::commands::Command],
) -> Option<&'a crate::commands::Command> {
    crate::commands::find_command(command_name, commands)
        .filter(|command| command.mcp_prompt.is_some())
}

/// Maps to: CC `client.ts:161-170` `McpSessionExpiredError` — the server
/// dropped the session and the connection cache was cleared; the caller gets
/// a fresh client through `ensureConnectedClient` and retries.
#[derive(Debug)]
pub struct McpSessionExpiredError {
    server_name: String,
}

impl McpSessionExpiredError {
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
        }
    }
}

impl std::fmt::Display for McpSessionExpiredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP server \"{}\" session expired", self.server_name)
    }
}

impl std::error::Error for McpSessionExpiredError {}

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

/// Maps to the callback result of getMcpToolsCommandsAndResources and
/// reconnectMcpServerImpl. Keep the actual capability payload separately from
/// the connection record: resource helper tools are callback-local.
#[derive(Clone, Debug)]
pub struct McpConnectionDiscovery {
    pub server: McpServerConnection,
    pub tools: Vec<crate::types::tools::Tool>,
    pub commands: Vec<crate::commands::Command>,
    pub resources: Option<Vec<super::types::ServerResource>>,
}

/// A connection with nothing fetched from it: CC's
/// `onConnectionAttempt({ client, tools: [], commands: [] })`.
impl From<McpServerConnection> for McpConnectionDiscovery {
    fn from(server: McpServerConnection) -> Self {
        Self {
            server,
            tools: Vec::new(),
            commands: Vec::new(),
            resources: None,
        }
    }
}

/// CC `client.type === 'connected' && client.capabilities?.resources`.
fn supports_resources(server: &McpServerConnection) -> bool {
    server
        .as_connected()
        .is_some_and(|server| server.capabilities.resources.is_some())
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
        if supports_resources(&self.server)
            && !added.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.append_resource_tools();
        }
    }

    fn add_reconnect_resource_tools(&mut self) {
        if self.server.connection_type() != McpServerConnectionType::Connected {
            self.tools.clear();
            self.commands.clear();
            self.resources = None;
            return;
        }
        if supports_resources(&self.server)
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
    pub clients: Vec<McpServerConnection>,
    pub tools: Vec<crate::types::tools::Tool>,
}

impl McpConnectionDiscovery {
    pub fn failed(
        name: impl Into<String>,
        config: &ScopedMcpServerConfig,
        error: impl Into<String>,
    ) -> Self {
        Self::from(McpServerConnection::failed(
            name,
            config.clone(),
            Some(error.into()),
        ))
    }

    /// Maps to: CC `useManageMCPConnections.ts` pending-client AppState rows.
    pub fn pending(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        Self::from(McpServerConnection::pending(name, config.clone()))
    }

    /// Maps to: CC `useManageMCPConnections.ts` disabled-client AppState rows.
    pub fn disabled(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        Self::from(McpServerConnection::disabled(name, config.clone()))
    }

    /// Maps to: CC `processServer`'s needs-auth result (`client.ts:2312-2333`):
    /// the connection plus `createMcpAuthTool(name, config)`.
    pub fn needs_auth(name: impl Into<String>, config: &ScopedMcpServerConfig) -> Self {
        let name = name.into();
        let mut discovery = Self::from(McpServerConnection::needs_auth(
            name.clone(),
            config.clone(),
        ));
        discovery.tools =
            vec![crate::tools::mcp_auth_tool::create_mcp_auth_tool(&name, config).tool()];
        discovery
    }
}

/// Maps to: CC `services/mcp/client.ts:2214-2225#processBatched`.
/// `pMap`'s returned array is discarded: each processor publishes its own
/// completion, and a finished item immediately releases its concurrency slot.
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

mod runtime {
    use super::*;
    use crate::services::mcp::sdk_control_transport::{
        SdkControlClientTransport, SendMcpMessageCallback,
    };
    use futures::StreamExt;
    use futures::stream::{self, BoxStream};
    use http::{HeaderName, HeaderValue};
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientNotification,
        CustomNotification, ElicitRequestParams as RmcpElicitRequestParams,
        ElicitResult as RmcpElicitResult, ElicitationAction as RmcpElicitationAction,
        ElicitationCapability, ErrorCode, GetPromptRequestParams, Implementation,
        InitializeRequestParams, ProtocolVersion, ReadResourceRequestParams, RequestMetaObject,
        RootsCapabilities,
    };
    // SEP-2577 deprecates roots in the 2026-07-28 draft; CC's SDK (1.29)
    // still serves `roots/list`, on the 2025-11-25 protocol Nyan speaks.
    #[allow(deprecated)]
    use rmcp::model::{ListRootsResult, Root};
    use rmcp::service::{
        ClientInitializeError, NotificationContext, Peer, RequestContext, RunningService,
        RunningServiceCancellationToken, RxJsonRpcMessage, ServiceError, TxJsonRpcMessage,
    };
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransportConfig, StreamableHttpError,
    };
    use rmcp::transport::{
        DynamicTransportError, StreamableHttpClientTransport, TokioChildProcess,
        Transport as RmcpTransport,
    };
    use rmcp::{ErrorData as McpError, RoleClient};
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, HashMap};
    use std::fmt;
    use std::future::Future;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, LazyLock, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::process::Command;
    use tokio::sync::oneshot;

    const DEFAULT_MCP_CONNECTION_TIMEOUT_MS: u64 = 30_000;
    const MCP_AUTH_CACHE_TTL_MS: u64 = 15 * 60 * 1000;
    const MAX_URL_ELICITATION_RETRIES: usize = 3;
    /// CC SDK `ErrorCode.UrlElicitationRequired` (`types.ts`). rmcp dropped
    /// the constant in 3.x along with its URL-elicitation error helpers.
    pub(super) const URL_ELICITATION_REQUIRED: ErrorCode = ErrorCode(-32042);
    /// CC SDK `LATEST_PROTOCOL_VERSION`, the version `Client.connect` sends
    /// in `initialize`. rmcp's default (`ProtocolVersion::LATEST`) is the
    /// 2026-07-28 draft, which has no `initialize` handshake at all.
    const LATEST_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

    /// CC `client.connect(transport)`: the `initialize` handshake.
    ///
    /// rmcp's client also keeps a response cache (SEP-2549) that the SDK CC
    /// runs on (1.29) does not have: `resources/read` and the `*/list`
    /// helpers answer from it while a server's `ttlMs` lasts, and fall back
    /// to a stale entry when a refetch fails. CC sends every request and
    /// surfaces every failure, so the cache is off for every connection.
    async fn serve_client<T, E, A>(
        handler: NyanMcpClientHandler,
        transport: T,
    ) -> Result<RunningService<RoleClient, NyanMcpClientHandler>, ClientInitializeError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        // Boxed: rmcp 3's handshake future is deep enough that inlining it
        // into every caller's state machine overflows rustc's layout query
        // depth (`cli::print::run_stream_json`).
        let service = Box::pin(rmcp::serve_client(handler, transport)).await?;
        service
            .peer()
            .set_response_cache_config(rmcp::ClientCacheConfig::disabled())
            .await;
        Ok(service)
    }

    static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

    /// A connection being made or made, shared by every caller that asks for
    /// it: CC's memoized promise.
    type SharedConnection =
        futures::future::Shared<futures::future::BoxFuture<'static, McpServerConnection>>;

    /// One [`connect_to_server`] memo entry. `connection_id` is assigned when
    /// the entry is made, so the connection is made under it and its close
    /// deletes exactly this entry.
    struct MemoizedConnection {
        config: ScopedMcpServerConfig,
        connection_id: u64,
        connection: SharedConnection,
    }

    /// CC `connectToServer`'s lodash `memoize` cache (`client.ts:595`), keyed
    /// by `getServerCacheKey(name, serverRef)` (`:581-586`): the name, then
    /// the config.
    static CONNECT_TO_SERVER_CACHE: LazyLock<
        std::sync::Mutex<HashMap<String, Vec<MemoizedConnection>>>,
    > = LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

    fn connect_to_server_cache()
    -> std::sync::MutexGuard<'static, HashMap<String, Vec<MemoizedConnection>>> {
        CONNECT_TO_SERVER_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `connectToServer.cache.delete(key)` for one connection (`client.ts:1396`
    /// in its `onclose`): only its own entry, so a newer connection made under
    /// the same key stays cached.
    fn delete_memoized_connection(name: &str, connection_id: u64) {
        let mut cache = connect_to_server_cache();
        let Some(entries) = cache.get_mut(name) else {
            return;
        };
        entries.retain(|entry| entry.connection_id != connection_id);
        if entries.is_empty() {
            cache.remove(name);
        }
    }

    /// The entries `clearServerCache` deletes (`client.ts:1666`): `server_ref`'s,
    /// or every config under `name` when the caller names none.
    fn take_memoized_connections(
        name: &str,
        server_ref: Option<&ScopedMcpServerConfig>,
    ) -> Vec<SharedConnection> {
        let mut cache = connect_to_server_cache();
        let Some(entries) = cache.get_mut(name) else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        entries.retain(|entry| {
            let matches = server_ref.is_none_or(|config| config == &entry.config);
            if matches {
                taken.push(entry.connection.clone());
            }
            !matches
        });
        if entries.is_empty() {
            cache.remove(name);
        }
        taken
    }

    /// Maps to: CC a connected server's SDK `Client`
    /// (`ConnectedMCPServer.client`, `services/mcp/types.ts:181`). Clones
    /// share the one connection, and equality is identity, as JS compares the
    /// `Client` object by reference. The name and config are what
    /// `connectToServer`'s closures capture (`name`, `serverRef`);
    /// [`McpClient::cancel`] is `client.close()`, and the rest of the
    /// connection's `cleanup()` (`client.ts:1404-1580`) is
    /// [`close_connection`].
    #[derive(Clone)]
    pub struct McpClient(Arc<McpClientInner>);

    struct McpClientInner {
        /// The connection's identity outside the handle: a memoized
        /// connection's entry carries it from before the connection is made,
        /// and the fetch caches mark their entries with it.
        connection_id: u64,
        name: String,
        config: ScopedMcpServerConfig,
        /// `None` only for a test handle with no connection behind it; every
        /// request on such a handle fails as not connected.
        peer: Option<Peer<RoleClient>>,
        /// Taken by whichever ends the connection first: [`McpClient::cancel`],
        /// or its close watcher once it has quit on its own. Gone, the
        /// connection is closed.
        cancellation_token: std::sync::Mutex<Option<RunningServiceCancellationToken>>,
        /// CC `client.onclose`: none until `onConnectionAttempt` assigns its
        /// handler (`useManageMCPConnections.ts:333`), and none again once
        /// `client.onclose = undefined` (`:809`) clears it. A connection that
        /// never goes through `onConnectionAttempt` — an agent's, one
        /// `ensureConnectedClient` makes — has none.
        onclose: std::sync::Mutex<Option<OncloseHandler>>,
        /// SDK `Client.setNotificationHandler`'s handlers, by method.
        notification_handlers: std::sync::Mutex<HashMap<String, NotificationHandler>>,
        /// SDK `Client.setRequestHandler(ElicitRequestSchema, ..)`'s handler.
        /// None stands for the default `connectToServer` registers, which
        /// cancels (`client.ts:1188-1197`).
        elicitation_handler: std::sync::Mutex<Option<ElicitationHandler>>,
    }

    /// A handler [`McpClient::set_elicitation_handler`] registers: it gets the
    /// request's params, its id, and a future that completes when the request
    /// is cancelled (the SDK's `extra.signal`), and answers the request.
    pub(crate) type ElicitationHandler = Arc<
        dyn Fn(
                RmcpElicitRequestParams,
                String,
                futures::future::BoxFuture<'static, ()>,
            ) -> futures::future::BoxFuture<'static, ElicitationResult>
            + Send
            + Sync,
    >;

    /// A handler [`McpClient::set_notification_handler`] registers: it gets
    /// the notification's params, and its work is the future it returns, as
    /// an SDK handler's is the promise it returns.
    pub(super) type NotificationHandler =
        Arc<dyn Fn(Option<Value>) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

    /// A handler [`McpClient::set_onclose`] assigns.
    pub(crate) type OncloseHandler =
        Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

    impl McpClient {
        fn new(
            connection_id: u64,
            name: String,
            config: ScopedMcpServerConfig,
            service: &RunningService<RoleClient, NyanMcpClientHandler>,
        ) -> Self {
            let client = Self(Arc::new(McpClientInner {
                connection_id,
                name,
                config,
                peer: Some(service.peer().clone()),
                cancellation_token: std::sync::Mutex::new(Some(service.cancellation_token())),
                onclose: std::sync::Mutex::new(None),
                notification_handlers: std::sync::Mutex::new(HashMap::new()),
                elicitation_handler: std::sync::Mutex::new(None),
            }));
            let _ = service.service().client.set(Arc::downgrade(&client.0));
            client
        }

        /// A handle with no connection behind it, for records that only need
        /// to say they are connected.
        #[cfg(test)]
        pub(crate) fn detached_for_test(name: &str, config: &ScopedMcpServerConfig) -> Self {
            Self(Arc::new(McpClientInner {
                connection_id: NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
                name: name.to_owned(),
                config: config.clone(),
                peer: None,
                cancellation_token: std::sync::Mutex::new(None),
                onclose: std::sync::Mutex::new(None),
                notification_handlers: std::sync::Mutex::new(HashMap::new()),
                elicitation_handler: std::sync::Mutex::new(None),
            }))
        }

        pub(crate) fn connection_id(&self) -> u64 {
            self.0.connection_id
        }

        pub(crate) fn name(&self) -> &str {
            &self.0.name
        }

        pub(crate) fn config(&self) -> &ScopedMcpServerConfig {
            &self.0.config
        }

        /// The connection's peer; a test handle has none and is not
        /// connected.
        fn peer(&self) -> anyhow::Result<&Peer<RoleClient>> {
            self.0
                .peer
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("MCP server \"{}\" is not connected", self.0.name))
        }

        /// Maps to: SDK `Client.notification({ method, params })`. A failed
        /// send reaches the connection's `onerror` first.
        pub(crate) async fn notification(&self, method: &str, params: Value) -> anyhow::Result<()> {
            let notification = CustomNotification::new(method.to_string(), Some(params));
            self.peer()?
                .send_notification(ClientNotification::CustomNotification(notification))
                .await
                .map_err(|error| request_failed(self, anyhow::Error::new(error)))
        }

        /// Maps to: SDK `Client.callTool(params, undefined, { timeout })`: the
        /// raw `tools/call` request, without `callMCPTool`'s handling.
        pub(crate) async fn call_tool(
            &self,
            params: CallToolRequestParams,
            timeout: Duration,
        ) -> anyhow::Result<CallToolResult> {
            match tokio::time::timeout(timeout, self.peer()?.call_tool(params)).await {
                Err(_) => anyhow::bail!("MCP error -32001: Request timed out"),
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(request_failed(self, anyhow::Error::new(error))),
            }
        }

        /// The connection's cancellation token, for the first of whoever
        /// closes the connection and its close watcher to ask: only that one
        /// does what follows the close.
        fn take_cancellation_token(&self) -> Option<RunningServiceCancellationToken> {
            self.0
                .cancellation_token
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        }

        /// Ends the connection unless it has ended already; its close watcher
        /// then sees it quit.
        fn cancel(&self) {
            if let Some(token) = self.take_cancellation_token() {
                token.cancel();
            }
        }

        /// Whether the connection has ended: cancelled here, or seen to quit
        /// by its close watcher. A test handle has no connection and is
        /// closed.
        pub(crate) fn is_closed(&self) -> bool {
            self.0
                .cancellation_token
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        }

        /// CC `client.onclose = handler`: replaces the one assigned before.
        /// Its close watcher runs it if the connection ends on its own.
        pub(crate) fn set_onclose(
            &self,
            handler: impl Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static,
        ) {
            *self
                .0
                .onclose
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(handler));
        }

        /// CC `client.onclose = undefined`.
        pub(crate) fn clear_onclose(&self) {
            *self
                .0
                .onclose
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }

        /// CC `client.onclose`.
        pub(crate) fn onclose(&self) -> Option<OncloseHandler> {
            self.0
                .onclose
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        /// Maps to: SDK `Client.setNotificationHandler(schema, handler)`
        /// (`shared/protocol.js:913-919`): one handler per method, a later
        /// one replacing the earlier.
        pub(crate) fn set_notification_handler(
            &self,
            method: &str,
            handler: impl Fn(Option<Value>) -> futures::future::BoxFuture<'static, ()>
            + Send
            + Sync
            + 'static,
        ) {
            self.0
                .notification_handlers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(method.to_owned(), Arc::new(handler));
        }

        /// Maps to: SDK `Client.removeNotificationHandler(method)`
        /// (`shared/protocol.js:923-925`); safe when none is registered.
        pub(crate) fn remove_notification_handler(&self, method: &str) {
            self.0
                .notification_handlers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(method);
        }

        /// Maps to: SDK `Client.setRequestHandler(ElicitRequestSchema,
        /// handler)` (`client/index.js:169-279`, over
        /// `shared/protocol.js:886-893`), replacing the handler registered
        /// before.
        pub(crate) fn set_elicitation_handler(
            &self,
            handler: impl Fn(
                RmcpElicitRequestParams,
                String,
                futures::future::BoxFuture<'static, ()>,
            ) -> futures::future::BoxFuture<'static, ElicitationResult>
            + Send
            + Sync
            + 'static,
        ) {
            *self
                .0
                .elicitation_handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(handler));
        }

        fn elicitation_handler(&self) -> Option<ElicitationHandler> {
            self.0
                .elicitation_handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        /// What a handler stored on this connection may hold of it.
        pub(crate) fn downgrade(&self) -> WeakMcpClient {
            WeakMcpClient(Arc::downgrade(&self.0))
        }

        pub(super) fn notification_handler(&self, method: &str) -> Option<NotificationHandler> {
            self.0
                .notification_handlers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(method)
                .cloned()
        }
    }

    /// A [`McpClient`] that does not keep the connection's handle alive. A
    /// handler stored on a connection holds its client this way: a JS
    /// closure holds the client itself and the garbage collector reclaims
    /// the cycle, which a strong handle here would leak.
    #[derive(Clone)]
    pub(crate) struct WeakMcpClient(std::sync::Weak<McpClientInner>);

    impl WeakMcpClient {
        /// The connection's connected record while its handle lives, built as
        /// `connectToServer` built it: equal to the record it made.
        pub(crate) fn connected_record(&self) -> Option<McpServerConnection> {
            let client = McpClient(self.0.upgrade()?);
            Some(McpServerConnection::Connected(connected_mcp_server(client)))
        }
    }

    impl PartialEq for McpClient {
        fn eq(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }
    }

    impl fmt::Debug for McpClient {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "McpClient(#{})", self.0.connection_id)
        }
    }

    /// Maps to: CC `connectToServer`'s connected result (`client.ts:1595-1604`):
    /// the server's capabilities (`{}` when it sent none), its `serverInfo`
    /// and its instructions, from what the server sent in the handshake. An
    /// SDK server's record has capabilities only (`setupSdkMcpClients`,
    /// `:3301-3310`).
    fn connected_mcp_server(client: McpClient) -> ConnectedMcpServer {
        let info = client.peer().ok().and_then(|peer| peer.peer_info());
        let sdk = client.config().transport == Transport::Sdk;
        ConnectedMcpServer {
            name: client.name().to_owned(),
            capabilities: info
                .as_ref()
                .map(|info| info.capabilities.clone())
                .unwrap_or_default(),
            server_info: info
                .as_ref()
                .filter(|_| !sdk)
                .and_then(|info| info.server_info.clone()),
            instructions: info
                .as_ref()
                .filter(|_| !sdk)
                .and_then(|info| info.instructions.as_deref())
                .map(truncate_mcp_instructions),
            config: client.config().clone(),
            client,
        }
    }

    /// Maps to: CC `client.ts:1159-1171`: instructions longer than
    /// `MAX_MCP_DESCRIPTION_LENGTH` UTF-16 code units are cut there and
    /// marked.
    pub(super) fn truncate_mcp_instructions(raw: &str) -> String {
        if raw.encode_utf16().count() <= MAX_MCP_DESCRIPTION_LENGTH {
            return raw.to_owned();
        }
        let kept = raw
            .encode_utf16()
            .take(MAX_MCP_DESCRIPTION_LENGTH)
            .collect::<Vec<_>>();
        format!("{}… [truncated]", String::from_utf16_lossy(&kept))
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct McpAuthCacheEntry {
        timestamp: u64,
    }

    type McpAuthCacheData = BTreeMap<String, McpAuthCacheEntry>;

    #[derive(Clone, Debug)]
    struct NyanMcpClientHandler {
        server_name: String,
        cwd: PathBuf,
        /// The connection this handler serves, set when its handle is made:
        /// its notifications go to the handlers registered on it. Weak, so
        /// the handler does not keep the handle alive on its own.
        client: Arc<OnceLock<std::sync::Weak<McpClientInner>>>,
    }

    impl NyanMcpClientHandler {
        fn new(server_name: &str) -> Self {
            Self {
                server_name: server_name.to_string(),
                cwd: std::env::current_dir().unwrap_or_default(),
                client: Arc::new(OnceLock::new()),
            }
        }

        /// SDK `Protocol._onnotification` (`shared/protocol.js:273-283`): the
        /// handler registered on this client for `method`. Without one the
        /// SDK drops the notification.
        fn registered_handler(&self, method: &str) -> Option<NotificationHandler> {
            McpClient(self.client.get()?.upgrade()?).notification_handler(method)
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

    impl ClientHandler for NyanMcpClientHandler {
        fn get_info(&self) -> InitializeRequestParams {
            // Maps to: CC `new Client({ name:'claude-code', title:'Claude Code', ... },
            // { capabilities:{ roots:{}, elicitation:{} } })`, initialized at the
            // SDK's `LATEST_PROTOCOL_VERSION`.
            let mut implementation =
                Implementation::new("nyan", crate::constants::product::VERSION);
            implementation.title = Some("nyan".to_string());
            implementation.website_url = Some(crate::constants::product::PRODUCT_URL.to_string());

            let mut capabilities = ClientCapabilities::default();
            capabilities.roots = Some(RootsCapabilities::default());
            // CC intentionally sends an empty elicitation object, not
            // form/url subcapabilities, for Java MCP SDK compatibility.
            capabilities.elicitation = Some(ElicitationCapability::default());

            InitializeRequestParams::new(capabilities, implementation)
                .with_protocol_version(LATEST_PROTOCOL_VERSION)
        }

        #[allow(deprecated)]
        fn list_roots(
            &self,
            _context: RequestContext<RoleClient>,
        ) -> impl Future<Output = Result<ListRootsResult, McpError>> + Send + '_ {
            let cwd = self.cwd.clone();
            async move {
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
            use futures::FutureExt as _;
            let server_name = self.server_name.clone();
            let handler = self
                .client
                .get()
                .and_then(std::sync::Weak::upgrade)
                .and_then(|client| McpClient(client).elicitation_handler());
            let request_id = context.id.to_string();
            let ct = context.ct.clone();
            async move {
                // SDK `Client.setRequestHandler`'s elicitation wrapper
                // (`client/index.js:191-207`): the client declares an empty
                // `elicitation: {}` (`get_info` above, CC `client.ts:999`),
                // which `getSupportedElicitationModes` (`:60-70`) reads as
                // form mode only, so a URL-mode request is refused before any
                // handler sees it.
                if matches!(
                    request,
                    RmcpElicitRequestParams::UrlElicitationParams { .. }
                ) {
                    return Err(McpError::invalid_params(
                        "Client does not support URL-mode elicitation requests",
                        None,
                    ));
                }
                let result = match handler {
                    Some(handler) => {
                        let cancelled = async move { ct.cancelled().await }.boxed();
                        handler(request, request_id, cancelled).await
                    }
                    // Maps to: CC the default handler `connectToServer`
                    // registers (`client.ts:1188-1197`), until
                    // `onConnectionAttempt` registers the real one.
                    None => {
                        crate::utils::log::log_mcp_debug(
                            &server_name,
                            &format!(
                                "Elicitation request received during initialization: {}",
                                elicitation_request_json(&request)
                            ),
                        );
                        ElicitationResult::new(ElicitationAction::Cancel)
                    }
                };
                Ok(elicitation_result_to_rmcp(result))
            }
        }

        // The three list_changed notifications go to the handler registered
        // for them on this client — `onConnectionAttempt` registers one per
        // list the server declared can change
        // (`useManageMCPConnections.ts:615-751`) — or nowhere.

        fn on_tool_list_changed(
            &self,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            use rmcp::model::ConstString as _;
            let handler =
                self.registered_handler(rmcp::model::ToolListChangedNotificationMethod::VALUE);
            async move {
                if let Some(handler) = handler {
                    handler(None).await;
                }
            }
        }

        fn on_prompt_list_changed(
            &self,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            use rmcp::model::ConstString as _;
            let handler =
                self.registered_handler(rmcp::model::PromptListChangedNotificationMethod::VALUE);
            async move {
                if let Some(handler) = handler {
                    handler(None).await;
                }
            }
        }

        fn on_resource_list_changed(
            &self,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            use rmcp::model::ConstString as _;
            let handler =
                self.registered_handler(rmcp::model::ResourceListChangedNotificationMethod::VALUE);
            async move {
                if let Some(handler) = handler {
                    handler(None).await;
                }
            }
        }

        fn on_custom_notification(
            &self,
            notification: CustomNotification,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let server_name = self.server_name.clone();
            let registered = self.registered_handler(&notification.method);
            async move {
                if crate::services::mcp::vscode_sdk_mcp::handle_vscode_log_event_notification(
                    &server_name,
                    &notification,
                ) {
                    return;
                }
                if let Some(handler) = registered {
                    handler(notification.params).await;
                }
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
        let client = crate::utils::proxy::get_proxy_fetch_options()?.build()?;
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
                    if let Some(data) = event.data.as_deref()
                        && let Some(message) = parse_mcp_rx_message(name, data)
                    {
                        initial_messages.push(message);
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
        crate::utils::env_utils::get_config_home_dir().join("mcp-needs-auth-cache.json")
    }

    fn remote_transport_uses_auth_cache(transport: Transport) -> bool {
        matches!(transport, Transport::Sse | Transport::Http)
    }

    fn read_mcp_auth_cache() -> McpAuthCacheData {
        std::fs::read_to_string(mcp_auth_cache_path())
            .ok()
            .and_then(|content| serde_json::from_str::<McpAuthCacheData>(&content).ok())
            .unwrap_or_default()
    }

    pub(super) fn is_mcp_auth_cached(server_id: &str) -> bool {
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
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        let (transport, _stderr) = TokioChildProcess::builder(stdio_command(config)?)
            .stderr(Stdio::piped())
            .spawn()?;
        let handler = NyanMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
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
        let session_bearer = crate::utils::session_ingress_auth::get_session_ingress_auth_token();
        let has_static_authorization = headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("authorization"));
        let auth_header =
            streamable_http_auth_header(oauth_token, session_bearer, has_static_authorization);

        // rmcp re-initializes an expired session in the transport by default.
        // The SDK transport CC uses throws instead, and the tool call clears
        // the connection and retries on a fresh one (`client.ts:3210-3231`,
        // `:1912-1922`).
        Ok(
            StreamableHttpClientTransportConfig::with_uri(url.to_string())
                .custom_headers(header_map_from_strings(headers)?)
                .reinit_on_expired_session(false)
                .auth_header(auth_header.unwrap_or_default()),
        )
    }

    async fn serve_streamable_http(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        let mut transport_config = streamable_http_config(name, config, url).await?;
        if transport_config.auth_header.as_deref() == Some("") {
            transport_config.auth_header = None;
        }
        let transport =
            StreamableHttpClientTransport::with_client(streamable_http_client()?, transport_config);
        let handler = NyanMcpClientHandler::new(name);
        let timeout = Duration::from_millis(connection_timeout_ms());
        Ok(tokio::time::timeout(timeout, serve_client(handler, transport)).await??)
    }

    /// The client rmcp's Streamable HTTP transport sends with. CC spreads
    /// `getProxyFetchOptions()` into the transport's `requestInit`
    /// (`client.ts:815-831`, `:887`). rmcp's own default has no idle pool and
    /// follows no redirect (`default_http_client`); both are kept.
    pub(super) fn streamable_http_client() -> anyhow::Result<reqwest::Client> {
        Ok(crate::utils::proxy::get_proxy_fetch_options()?
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()?)
    }

    async fn serve_legacy_sse(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
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
        // CC races `client.connect(transport)`, which starts the transport,
        // against the connection timeout (`client.ts:1048-1080`).
        let timeout = Duration::from_millis(connection_timeout_ms());
        tokio::time::timeout(timeout, async {
            let transport = legacy_sse_transport(name, url, headers).await?;
            let handler = NyanMcpClientHandler::new(name);
            anyhow::Ok(serve_client(handler, transport).await?)
        })
        .await?
    }

    async fn serve_legacy_sse_ide(
        name: &str,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        let headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        let timeout = Duration::from_millis(connection_timeout_ms());
        tokio::time::timeout(timeout, async {
            let transport = legacy_sse_transport(name, url, headers).await?;
            let handler = NyanMcpClientHandler::new(name);
            anyhow::Ok(serve_client(handler, transport).await?)
        })
        .await?
    }

    async fn serve_websocket(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        if let Some(token) = crate::utils::session_ingress_auth::get_session_ingress_auth_token() {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        headers.extend(
            crate::services::mcp::headers_helper::get_mcp_server_headers(name, config).await,
        );
        let timeout = Duration::from_millis(connection_timeout_ms());
        tokio::time::timeout(timeout, async {
            let transport = crate::utils::mcp_websocket_transport::connect_mcp_websocket_transport(
                name, url, headers,
            )
            .await?;
            let handler = NyanMcpClientHandler::new(name);
            anyhow::Ok(serve_client(handler, transport).await?)
        })
        .await?
    }

    async fn serve_websocket_ide(
        name: &str,
        config: &ScopedMcpServerConfig,
        url: &str,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        let mut headers = BTreeMap::from([(
            "User-Agent".to_string(),
            crate::utils::http::get_mcp_user_agent(),
        )]);
        // CC `...(serverRef.authToken && { ... })` (`client.ts:712-714`):
        // any non-empty token, whitespace too.
        if let Some(auth_token) = config
            .auth_token
            .as_deref()
            .filter(|token| !token.is_empty())
        {
            headers.insert(
                "X-Claude-Code-Ide-Authorization".to_string(),
                auth_token.to_string(),
            );
        }
        let timeout = Duration::from_millis(connection_timeout_ms());
        tokio::time::timeout(timeout, async {
            let transport = crate::utils::mcp_websocket_transport::connect_mcp_websocket_transport(
                name, url, headers,
            )
            .await?;
            let handler = NyanMcpClientHandler::new(name);
            anyhow::Ok(serve_client(handler, transport).await?)
        })
        .await?
    }

    async fn serve_transport(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
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
            if let Some(error) = cause.downcast_ref::<StreamableHttpError<reqwest::Error>>()
                && let Some(challenge) = streamable_http_challenge_from_error(error)
            {
                return Some(challenge);
            }
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>()
                && let Some(header) = error.www_authenticate.clone()
            {
                return Some((header.clone(), extract_www_auth_scope_param(&header)));
            }
            if let Some(ClientInitializeError::TransportError { error, .. }) =
                cause.downcast_ref::<ClientInitializeError>()
                && let Some(challenge) = streamable_http_challenge_from_dynamic_error(error)
            {
                return Some(challenge);
            }
            if let Some(ServiceError::TransportSend(error)) = cause.downcast_ref::<ServiceError>()
                && let Some(challenge) = streamable_http_challenge_from_dynamic_error(error)
            {
                return Some(challenge);
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
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>()
                && error.status == 403
                && error
                    .www_authenticate
                    .as_deref()
                    .is_some_and(|header| header.contains("insufficient_scope"))
            {
                return true;
            }
            if let Some(ClientInitializeError::TransportError { error, .. }) =
                cause.downcast_ref::<ClientInitializeError>()
                && let Some(StreamableHttpError::InsufficientScope(_)) =
                    error
                        .error
                        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
            {
                return true;
            }
            if let Some(ServiceError::TransportSend(error)) = cause.downcast_ref::<ServiceError>()
                && let Some(StreamableHttpError::InsufficientScope(_)) =
                    error
                        .error
                        .downcast_ref::<StreamableHttpError<reqwest::Error>>()
            {
                return true;
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
            && let Err(record_error) =
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
                .downcast_ref::<crate::constants::oauth::OAuthCredentialSideEffectsUnavailable>()
                .is_some()
            {
                return Err(record_error);
            }
            tracing::debug!(server = name, error = %record_error, "failed to persist MCP WWW-Authenticate challenge");
        }
        Ok(())
    }

    fn remote_auth_context(config: &ScopedMcpServerConfig) -> Option<(&'static str, &str)> {
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
    ) -> anyhow::Result<RunningService<RoleClient, NyanMcpClientHandler>> {
        match serve_transport(name, config).await {
            Ok(service) => Ok(service),
            Err(error) if is_auth_error(&error) => {
                record_remote_auth_challenge(name, config, &error)?;
                if refresh_after_remote_auth_failure(name, config, &error).await? {
                    match serve_transport(name, config).await {
                        Ok(service) => Ok(service),
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
            if let Some(error) = cause.downcast_ref::<McpRemoteAuthHttpError>()
                && matches!(error.status, 401 | 403)
            {
                return true;
            }
            if let Some(error) = cause.downcast_ref::<reqwest::Error>()
                && error
                    .status()
                    .is_some_and(|status| matches!(status.as_u16(), 401 | 403))
            {
                return true;
            }
            if matches!(
                cause.downcast_ref::<StreamableHttpError<reqwest::Error>>(),
                Some(StreamableHttpError::AuthRequired(_))
                    | Some(StreamableHttpError::InsufficientScope(_))
            ) {
                return true;
            }
        }
        // A JSON-RPC error response carries a JSON-RPC code, never an HTTP
        // status: CC's `errorCode === 401 || e instanceof UnauthorizedError`
        // (`client.ts:3197-3199`) cannot match one, whatever its message or
        // data say — a URL elicitation's URL included.
        if mcp_error_from_anyhow(error).is_some() {
            return false;
        }
        let message = error.to_string().to_ascii_lowercase();
        message.contains("401")
            || message.contains("unauthorized")
            || message.contains("authrequired")
            || message.contains("auth required")
            || message.contains("insufficient scope")
    }

    /// Maps to: CC `client.ts:193-206` `isMcpSessionExpiredError`: a 404 to a
    /// request that carried a session id — rmcp's
    /// `StreamableHttpError::SessionExpired`, which reaches the caller inside
    /// `ServiceError::TransportSend`. CC also requires the body's JSON-RPC
    /// code -32001; rmcp returns that error before reading the body.
    fn is_mcp_session_expired_error(error: &anyhow::Error) -> bool {
        let session_expired = |cause: &(dyn std::error::Error + 'static)| {
            matches!(
                cause.downcast_ref::<StreamableHttpError<reqwest::Error>>(),
                Some(StreamableHttpError::SessionExpired)
            )
        };
        error.chain().any(|cause| {
            session_expired(cause)
                || matches!(
                    cause.downcast_ref::<ServiceError>(),
                    Some(ServiceError::TransportSend(transport))
                        if session_expired(transport.error.as_ref())
                )
        })
    }

    /// Maps to: CC `callMCPTool`'s session-expiry test (`client.ts:3210-3223`):
    /// [`is_mcp_session_expired_error`], or the SDK's -32000 "Connection
    /// closed" — rmcp's `ServiceError::TransportClosed` — for a call whose
    /// `http` transport closed under it.
    fn is_tool_call_session_expired_error(
        error: &anyhow::Error,
        config: &ScopedMcpServerConfig,
    ) -> bool {
        is_mcp_session_expired_error(error)
            || (config.transport == Transport::Http
                && error.chain().any(|cause| {
                    matches!(
                        cause.downcast_ref::<ServiceError>(),
                        Some(ServiceError::TransportClosed)
                    )
                }))
    }

    /// Maps to: CC the connection's `onerror` session-expiry branch
    /// (`client.ts:1313-1327`): an `http` request that
    /// failed on an expired session closes its transport, whose `onclose`
    /// drops the connection from the memo and the fetch caches — whatever the
    /// request was, so the next `ensureConnectedClient` reconnects.
    fn close_on_expired_session(client: &McpClient, error: &anyhow::Error) {
        if client.config().transport == Transport::Http && is_mcp_session_expired_error(error) {
            tracing::debug!(
                server = client.name(),
                "MCP session expired (server returned 404 with session-not-found), triggering reconnection"
            );
            close_connection(client);
        }
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
        mcp_error_from_anyhow(error).is_some_and(|error| error.code == URL_ELICITATION_REQUIRED)
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

    /// `fetchCommandsForClient`'s map over one `prompts/list` entry; `argNames`
    /// is `Object.values(prompt.arguments ?? {}).map(k => k.name)`.
    fn prompt_command(
        client: &std::sync::Arc<ConnectedMcpServer>,
        prompt: rmcp::model::Prompt,
    ) -> crate::commands::Command {
        let arg_names = prompt
            .arguments
            .unwrap_or_default()
            .into_iter()
            .map(|argument| argument.name)
            .collect();
        crate::commands::Command::from_mcp_prompt(
            client,
            &prompt.name,
            prompt.description.as_deref(),
            arg_names,
        )
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

    /// Maps to: CC `services/mcp/client.ts:1726` `MCP_FETCH_CACHE_SIZE`.
    const MCP_FETCH_CACHE_SIZE: usize = 20;

    /// Maps to: MCP SDK `shared/protocol.js:8` `DEFAULT_REQUEST_TIMEOUT_MSEC`,
    /// the timeout `client.request` applies to the `fetch*ForClient` list
    /// requests, which pass none of their own.
    const MCP_REQUEST_TIMEOUT: Duration = Duration::from_millis(60_000);

    /// A memoized fetch: the connection it fetches from, and the fetch, made
    /// or being made, shared by every caller that asks for it — CC's cached
    /// promise.
    type MemoizedFetch<T> = (
        u64,
        futures::future::Shared<futures::future::BoxFuture<'static, Vec<T>>>,
    );

    /// Keyed by the client's name, as CC's `client => client.name`.
    type FetchForClient<T> =
        crate::utils::memoize::LruMemoizedFunction<McpClient, MemoizedFetch<T>>;

    /// The connection a `fetch*ForClient` call fetches from: `client`'s,
    /// while it is open. `None` when it is not connected (CC's `client.type
    /// !== 'connected'` early return), or its connection has closed — CC
    /// would request on the closed client and cache the failure as `[]`.
    fn connected_client_for_fetch(client: &McpServerConnection) -> Option<McpClient> {
        let client = &client.as_connected()?.client;
        (!client.is_closed()).then(|| client.clone())
    }

    /// Maps to: CC `memoizeWithLRU(fetch, client => client.name,
    /// MCP_FETCH_CACHE_SIZE)` around each `fetch*ForClient`. The cached value
    /// is the fetch itself, run on the process runtime, so it proceeds and
    /// completes like a JS promise whether or not the caller that started it
    /// is still waiting.
    fn memoize_fetch<T, Fut>(
        fetch: impl Fn(McpClient) -> Fut + Send + Sync + 'static,
    ) -> FetchForClient<T>
    where
        T: Clone + Send + Sync + 'static,
        Fut: Future<Output = Vec<T>> + Send + 'static,
    {
        crate::utils::memoize::memoize_with_lru(
            move |client: &McpClient| {
                use futures::FutureExt as _;
                let fetch = fetch(client.clone());
                let fetch = match crate::utils::process_runtime::runtime_handle_for_detached_work()
                {
                    Some(handle) => {
                        let task = handle.spawn(fetch);
                        async move { task.await.unwrap_or_default() }
                            .boxed()
                            .shared()
                    }
                    None => fetch.boxed().shared(),
                };
                (client.connection_id(), fetch)
            },
            |client| client.name().to_owned(),
            Some(MCP_FETCH_CACHE_SIZE),
        )
    }

    /// Maps to: CC `client.ts:1743` `fetchToolsForClient`.
    static FETCH_TOOLS_FOR_CLIENT: LazyLock<FetchForClient<crate::types::tools::Tool>> =
        LazyLock::new(|| memoize_fetch(fetch_tools));

    /// Maps to: CC `client.ts:2033` `fetchCommandsForClient`.
    static FETCH_COMMANDS_FOR_CLIENT: LazyLock<FetchForClient<crate::commands::Command>> =
        LazyLock::new(|| memoize_fetch(fetch_commands));

    /// Maps to: CC `client.ts:2000` `fetchResourcesForClient`.
    static FETCH_RESOURCES_FOR_CLIENT: LazyLock<FetchForClient<ServerResource>> =
        LazyLock::new(|| memoize_fetch(fetch_resources));

    /// `memoized(client)`. A cached fetch from another connection under the
    /// name is a miss: CC's entries never outlive their connection's
    /// `onclose`, and two connections under one name with different configs
    /// each fetch their own, where CC's name-only key would hand one the
    /// other's tools, bound to the other's client.
    ///
    /// Without a process runtime the fetch runs for this caller alone,
    /// uncached: a cached fetch would live on the caller's runtime, which may
    /// end with the turn.
    async fn fetch_through_cache<T, Fut>(
        memoized: &FetchForClient<T>,
        client: McpClient,
        fetch: impl FnOnce(McpClient) -> Fut,
    ) -> Vec<T>
    where
        T: Clone,
        Fut: Future<Output = Vec<T>>,
    {
        if crate::utils::process_runtime::runtime_handle_for_detached_work().is_none() {
            return fetch(client).await;
        }
        let (connection_id, cached) = memoized.call(&client);
        if connection_id == client.connection_id() {
            return cached.await;
        }
        memoized.cache.delete(client.name());
        memoized.call(&client).1.await
    }

    /// The name's entries in all three fetch caches: CC deletes them together
    /// when a connection closes (`client.ts:1389-1391`) or is cleared
    /// (`:1667-1669`).
    fn delete_fetch_caches(name: &str) {
        FETCH_TOOLS_FOR_CLIENT.cache.delete(name);
        FETCH_RESOURCES_FOR_CLIENT.cache.delete(name);
        FETCH_COMMANDS_FOR_CLIENT.cache.delete(name);
    }

    /// Maps to: MCP SDK `Protocol.request` (`shared/protocol.js:612-730`)
    /// without options, as the `fetch*ForClient` calls make it: after
    /// `DEFAULT_REQUEST_TIMEOUT_MSEC` the request is cancelled with
    /// `notifications/cancelled` and fails.
    async fn request_with_default_timeout(
        peer: &Peer<RoleClient>,
        request: rmcp::model::ClientRequest,
    ) -> Result<rmcp::model::ServerResult, ServiceError> {
        let options = rmcp::service::PeerRequestOptions::with_timeout(MCP_REQUEST_TIMEOUT);
        peer.send_request_with_option(request, options)
            .await?
            .await_response()
            .await
    }

    /// Maps to: CC `client.ts:1743-1998` `fetchToolsForClient`'s request: one
    /// `tools/list`, no cursor; a failure is logged and yields `[]`.
    async fn fetch_tools(client: McpClient) -> Vec<crate::types::tools::Tool> {
        // The record every tool of this fetch closes over.
        let connected = std::sync::Arc::new(connected_mcp_server(client.clone()));
        if connected.capabilities.tools.is_none() {
            return Vec::new();
        }
        let Ok(peer) = client.peer() else {
            return Vec::new();
        };
        let request = rmcp::model::ClientRequest::ListToolsRequest(rmcp::model::ListToolsRequest {
            method: Default::default(),
            params: None,
            extensions: Default::default(),
        });
        match request_with_default_timeout(peer, request).await {
            Ok(rmcp::model::ServerResult::ListToolsResult(result)) => {
                mcp_tools_from_server_tools(&connected, result.tools)
            }
            Ok(_) => {
                tracing::warn!(server = %client.name(), "unexpected response to MCP tools/list");
                Vec::new()
            }
            Err(error) => {
                let error = anyhow::Error::new(error);
                close_on_expired_session(&client, &error);
                tracing::warn!(server = %client.name(), error = %error, "failed to fetch MCP tools");
                Vec::new()
            }
        }
    }

    /// Maps to: CC `client.ts:2000-2031` `fetchResourcesForClient`'s request:
    /// one `resources/list`, each entry stamped with its server.
    async fn fetch_resources(client: McpClient) -> Vec<ServerResource> {
        let Ok(peer) = client.peer() else {
            return Vec::new();
        };
        if !peer
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some())
        {
            return Vec::new();
        }
        let request =
            rmcp::model::ClientRequest::ListResourcesRequest(rmcp::model::ListResourcesRequest {
                method: Default::default(),
                params: None,
                extensions: Default::default(),
            });
        match request_with_default_timeout(peer, request).await {
            Ok(rmcp::model::ServerResult::ListResourcesResult(result)) => result
                .resources
                .into_iter()
                .map(|resource| resource_snapshot(resource, client.name()))
                .collect(),
            Ok(_) => {
                tracing::warn!(server = %client.name(), "unexpected response to MCP resources/list");
                Vec::new()
            }
            Err(error) => {
                let error = anyhow::Error::new(error);
                close_on_expired_session(&client, &error);
                tracing::warn!(server = %client.name(), error = %error, "failed to fetch MCP resources");
                Vec::new()
            }
        }
    }

    /// Maps to: CC `client.ts:2033-2107` `fetchCommandsForClient`'s request:
    /// one `prompts/list`.
    async fn fetch_commands(client: McpClient) -> Vec<crate::commands::Command> {
        // The record every command of this fetch closes over.
        let connected = std::sync::Arc::new(connected_mcp_server(client.clone()));
        if connected.capabilities.prompts.is_none() {
            return Vec::new();
        }
        let Ok(peer) = client.peer() else {
            return Vec::new();
        };
        let request =
            rmcp::model::ClientRequest::ListPromptsRequest(rmcp::model::ListPromptsRequest {
                method: Default::default(),
                params: None,
                extensions: Default::default(),
            });
        match request_with_default_timeout(peer, request).await {
            Ok(rmcp::model::ServerResult::ListPromptsResult(result)) => result
                .prompts
                .into_iter()
                .map(|prompt| prompt_command(&connected, prompt))
                .collect(),
            Ok(_) => {
                tracing::warn!(server = %client.name(), "unexpected response to MCP prompts/list");
                Vec::new()
            }
            Err(error) => {
                let error = anyhow::Error::new(error);
                close_on_expired_session(&client, &error);
                tracing::warn!(server = %client.name(), error = %error, "failed to fetch MCP prompts");
                Vec::new()
            }
        }
    }

    /// Maps to: CC `client.ts:1743` `fetchToolsForClient(client)`.
    pub async fn fetch_tools_for_client(
        client: &McpServerConnection,
    ) -> Vec<crate::types::tools::Tool> {
        let Some(client) = connected_client_for_fetch(client) else {
            return Vec::new();
        };
        fetch_through_cache(&FETCH_TOOLS_FOR_CLIENT, client, fetch_tools).await
    }

    /// Maps to: CC `client.ts:2033` `fetchCommandsForClient(client)`.
    pub async fn fetch_commands_for_client(
        client: &McpServerConnection,
    ) -> Vec<crate::commands::Command> {
        let Some(client) = connected_client_for_fetch(client) else {
            return Vec::new();
        };
        fetch_through_cache(&FETCH_COMMANDS_FOR_CLIENT, client, fetch_commands).await
    }

    /// Maps to: CC `client.ts:2000` `fetchResourcesForClient(client)`.
    pub async fn fetch_resources_for_client(client: &McpServerConnection) -> Vec<ServerResource> {
        let Some(client) = connected_client_for_fetch(client) else {
            return Vec::new();
        };
        fetch_through_cache(&FETCH_RESOURCES_FOR_CLIENT, client, fetch_resources).await
    }

    /// Maps to: CC `fetchToolsForClient.cache.delete(client.name);
    /// fetchToolsForClient(client)` in the `tools/list_changed` handler
    /// (`useManageMCPConnections.ts:631-632`), `client` being the connection
    /// the notification came from.
    pub async fn refresh_mcp_tools_for_client(
        client: &McpServerConnection,
    ) -> Vec<crate::types::tools::Tool> {
        FETCH_TOOLS_FOR_CLIENT.cache.delete(client.name());
        fetch_tools_for_client(client).await
    }

    /// Maps to: CC `fetchCommandsForClient.cache.delete(client.name)` then
    /// `fetchCommandsForClient(client)` in the `prompts/list_changed` handler
    /// (`useManageMCPConnections.ts:681-683`).
    pub async fn refresh_mcp_prompts_for_client(
        client: &McpServerConnection,
    ) -> Vec<crate::commands::Command> {
        FETCH_COMMANDS_FOR_CLIENT.cache.delete(client.name());
        fetch_commands_for_client(client).await
    }

    /// Maps to: CC `fetchResourcesForClient.cache.delete(client.name)` then
    /// `fetchResourcesForClient(client)` in the `resources/list_changed`
    /// handler (`useManageMCPConnections.ts:717`, `:727-741`).
    pub async fn refresh_mcp_resources_for_client(
        client: &McpServerConnection,
    ) -> Vec<ServerResource> {
        FETCH_RESOURCES_FOR_CLIENT.cache.delete(client.name());
        fetch_resources_for_client(client).await
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
        let mut event = ElicitationRequestEvent::new(
            server_name.to_string(),
            request_id,
            rmcp_elicitation_params_to_event_params(request),
        );
        // `waitingState: { actionLabel: 'Retry now', showCancel: true }`
        // (`client.ts:2950-2953`).
        event.waiting_state = Some(ElicitationWaitingState {
            action_label: "Retry now".to_string(),
            show_cancel: true,
        });
        crate::services::mcp::use_manage_mcp_connections::emit_elicitation_requested(event).await;
        receiver
            .await
            .unwrap_or_else(|_| ElicitationResult::new(ElicitationAction::Cancel))
    }

    /// The tool result `callMCPToolWithUrlElicitationRetry` returns when a hook
    /// or the user does not accept (`client.ts:2936`, `:3014`).
    fn url_elicitation_required_result_message(
        action: ElicitationAction,
        source: &str,
        tool: &str,
    ) -> String {
        let verb = match action {
            ElicitationAction::Decline => "declined",
            ElicitationAction::Cancel => "canceled",
            ElicitationAction::Accept => "accepted",
        };
        format!(
            "URL elicitation was {verb} by {source}. The tool \"{tool}\" could not complete because it requires the user to open a URL."
        )
    }

    pub(super) async fn process_url_elicitation_required(
        server_name: &str,
        tool: &str,
        elicitations: Vec<RmcpElicitRequestParams>,
        handle_elicitation: crate::tool::HandleElicitationCallback,
    ) -> Option<Value> {
        for elicitation in elicitations {
            if let Some(hook_response) = run_elicitation_hooks(server_name, &elicitation).await {
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
            let elicitation_id = match &elicitation {
                RmcpElicitRequestParams::UrlElicitationParams { elicitation_id, .. } => {
                    Some(elicitation_id.as_str())
                }
                _ => None,
            };
            // `client.ts:3000-3006`: `runElicitationResultHooks(serverName,
            // userResult, signal, 'url', elicitationId)`.
            let final_result = run_elicitation_result_hooks(
                server_name,
                user_result,
                Some(ElicitationMode::Url),
                elicitation_id,
            )
            .await;
            if final_result.action != ElicitationAction::Accept {
                return Some(mcp_tool_text_result(
                    url_elicitation_required_result_message(final_result.action, "the user", tool),
                ));
            }
        }
        None
    }

    /// Watches `client`'s connection until it ends, then runs the `onclose`
    /// assigned to it, if any.
    ///
    /// CC's `onclose` fires on every close, `cleanup()` and an expired
    /// session's close included. Here it fires only for a connection that
    /// ended on its own: one closed on purpose (`cleanup`, an expired
    /// session, `clearServerCache`) does not, so a manual reconnect does not
    /// also start an automatic one.
    fn spawn_service_close_watcher(
        client: McpClient,
        service: RunningService<RoleClient, NyanMcpClientHandler>,
    ) {
        tokio::spawn(async move {
            let _ = service.waiting().await;
            // Whoever closed it on purpose took the token first and did the
            // rest already.
            if client.take_cancellation_token().is_none() {
                return;
            }
            // Maps to: CC `connectToServer`'s `onclose` wrapper
            // (`client.ts:1374-1402`): the connection takes the name's fetch
            // caches with it and leaves the memo.
            delete_fetch_caches(client.name());
            delete_memoized_connection(client.name(), client.connection_id());
            if let Some(onclose) = client.onclose() {
                onclose().await;
            }
        });
    }

    /// Watches a connection just made for its close — the rest of CC's
    /// `connectToServer` once `client.connect` resolves, which wraps the
    /// client's `onerror` and `onclose` (`client.ts:1216-1402`).
    ///
    /// A memoized connection whose memo entry a `clearServerCache` took while
    /// it was being made is closed at once instead, and `false` returned: CC's
    /// clear awaits the connect and then `cleanup()`s it. Closing it here,
    /// before the clear gets to it, hands its waiters a connection already
    /// closed.
    fn watch_connection(
        client: &McpClient,
        service: RunningService<RoleClient, NyanMcpClientHandler>,
        memoized: bool,
    ) -> bool {
        if memoized
            && !connect_to_server_cache()
                .get(client.name())
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.connection_id == client.connection_id())
                })
        {
            client.cancel();
            return false;
        }
        spawn_service_close_watcher(client.clone(), service);
        true
    }

    /// Maps to: CC a connected client's `cleanup()` (`client.ts:1404-1580`):
    /// closes that one connection. Its `onclose` wrapper drops it from the
    /// memo and the name's fetch caches; that happens here at once, as CC's
    /// close has run it by the time `cleanup()` resolves.
    pub fn cleanup_connection(client: &McpClient) {
        close_connection(client);
    }

    /// Closes `client`'s connection and does at once what its `onclose`
    /// wrapper would: drop it from the memo, and the name's fetch caches with
    /// it. A connection that had ended already is left alone; whoever ended
    /// it did both.
    ///
    /// The memo entry goes before the connection does, so no caller is handed
    /// the connection as it closes.
    fn close_connection(client: &McpClient) {
        let Some(token) = client.take_cancellation_token() else {
            return;
        };
        delete_memoized_connection(client.name(), client.connection_id());
        delete_fetch_caches(client.name());
        token.cancel();
    }

    /// `cleanup()`s the connection `connect_to_server(name, config)` is
    /// making, once it is made — CC's `runAgent` `finally` awaits the connect
    /// before its cleanup runs. Never connects anew: with no memo entry there
    /// is nothing to close.
    pub fn cleanup_memoized_connection(name: &str, config: &ScopedMcpServerConfig) {
        let connection = connect_to_server_cache().get(name).and_then(|entries| {
            entries
                .iter()
                .find(|entry| &entry.config == config)
                .map(|entry| entry.connection.clone())
        });
        let (Some(connection), Some(handle)) = (
            connection,
            crate::utils::process_runtime::runtime_handle_for_detached_work(),
        ) else {
            return;
        };
        handle.spawn(async move {
            if let McpServerConnection::Connected(connected) = connection.await {
                cleanup_connection(&connected.client);
            }
        });
    }

    /// Maps to: CC `client.ts:1648-1673` `clearServerCache`.
    ///
    /// Everything it clears is captured at invocation, before the returned
    /// future is polled — CC's memo lookup runs before its first await. The
    /// memo entries leave the cache then; each connection is awaited and
    /// closed, as CC awaits `connectToServer(name, serverRef)` before
    /// `cleanup()`. One still being made finds its entry gone and closes
    /// itself. The rest runs on the process runtime, so it completes like a
    /// JS promise whether or not the caller is still waiting: a dropped clear
    /// would leave the connections it took open, out of every memo.
    ///
    /// On a memo miss CC connects just to close the result again
    /// (`useManageMCPConnections.ts:798-801`); here there is nothing to close.
    /// Nor does it reach a connection made outside the memo: an SDK server's,
    /// which CC's `connectToServer` rejects (`client.ts:866-867`), so its
    /// `clearServerCache` never reaches their `cleanup()` either; and, with
    /// no process runtime, one made for its caller alone.
    ///
    /// `server_ref` is CC's; `None` clears every config under `name`.
    pub fn clear_server_cache(
        name: &str,
        server_ref: Option<&ScopedMcpServerConfig>,
    ) -> impl Future<Output = ()> + Send + 'static {
        use futures::FutureExt as _;
        let name = name.to_owned();
        let memoized = take_memoized_connections(&name, server_ref);
        let clear = async move {
            for connection in memoized {
                if let McpServerConnection::Connected(connected) = connection.await {
                    connected.client.cancel();
                }
            }
            delete_fetch_caches(&name);
        };
        match crate::utils::process_runtime::runtime_handle_for_detached_work() {
            Some(handle) => {
                let task = handle.spawn(clear);
                async move {
                    let _ = task.await;
                }
                .boxed()
            }
            None => clear.boxed(),
        }
    }

    /// The connection ids memoized under `name`.
    #[cfg(test)]
    pub(crate) fn memoized_connection_ids(name: &str) -> Vec<u64> {
        connect_to_server_cache()
            .get(name)
            .map(|entries| entries.iter().map(|entry| entry.connection_id).collect())
            .unwrap_or_default()
    }

    /// Maps to: CC `client.ts:595` `connectToServer`, memoized by
    /// `getServerCacheKey`: a call for a name and config already asked for
    /// gets the same connection, while it is being made and after — a failed
    /// one too — until `clearServerCache` or the connection's `onclose`
    /// deletes the entry. A subagent that references a server by name thus
    /// gets the parent's connection. The result is the connection alone;
    /// callers fetch its tools, prompts and resources through the
    /// `fetch*_for_client` caches, as CC's do.
    ///
    /// The connection is made on the process runtime, so a memoized
    /// connection outlives the turn that first asked for it, as it does in
    /// CC's single event loop. Without a process runtime it would live on the
    /// caller's runtime, which may end with the turn; it is then made for
    /// this caller alone, outside the memo.
    pub async fn connect_to_server(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpServerConnection {
        use futures::FutureExt as _;
        let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
            let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
            return establish_connection(name.to_owned(), config.clone(), connection_id, false)
                .await;
        };
        let connection = {
            let mut cache = connect_to_server_cache();
            let entries = cache.entry(name.to_owned()).or_default();
            match entries.iter().find(|entry| &entry.config == config) {
                Some(entry) => entry.connection.clone(),
                None => {
                    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
                    let task = handle.spawn(establish_connection(
                        name.to_owned(),
                        config.clone(),
                        connection_id,
                        true,
                    ));
                    let (failed_name, failed_config) = (name.to_owned(), config.clone());
                    let connection = async move {
                        task.await.unwrap_or_else(|error| {
                            McpConnectionDiscovery::failed(
                                failed_name,
                                &failed_config,
                                format!("MCP connection did not finish: {error}"),
                            )
                            .server
                        })
                    }
                    .boxed()
                    .shared();
                    entries.push(MemoizedConnection {
                        config: config.clone(),
                        connection_id,
                        connection: connection.clone(),
                    });
                    connection
                }
            }
        };
        connection.await
    }

    /// The body CC's `connectToServer` memoizes: connect, and keep the
    /// connection under `connection_id`.
    async fn establish_connection(
        name: String,
        config: ScopedMcpServerConfig,
        connection_id: u64,
        memoized: bool,
    ) -> McpServerConnection {
        let service = match serve_transport_with_auth_retry(&name, &config).await {
            Ok(service) => service,
            Err(error)
                if remote_transport_uses_auth_cache(config.transport) && is_auth_error(&error) =>
            {
                if let Err(gate_error) = record_remote_auth_challenge(&name, &config, &error) {
                    return McpConnectionDiscovery::failed(name, &config, gate_error.to_string())
                        .server;
                }
                set_mcp_auth_cache_entry(&name);
                return McpServerConnection::needs_auth(name, config);
            }
            Err(error) if is_auth_error(&error) => {
                if let Err(gate_error) = record_remote_auth_challenge(&name, &config, &error) {
                    return McpConnectionDiscovery::failed(name, &config, gate_error.to_string())
                        .server;
                }
                return McpServerConnection::needs_auth(name, config);
            }
            Err(error) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "[MCP] {name} connect error: {error}"
                ));
                return McpConnectionDiscovery::failed(name, &config, error.to_string()).server;
            }
        };

        let client = McpClient::new(connection_id, name.clone(), config, &service);
        let connected = connected_mcp_server(client.clone());
        if let Some(length) = service
            .peer()
            .peer_info()
            .and_then(|info| {
                info.instructions
                    .as_ref()
                    .map(|raw| raw.encode_utf16().count())
            })
            .filter(|length| *length > MAX_MCP_DESCRIPTION_LENGTH)
        {
            crate::utils::log::log_mcp_debug(
                &name,
                &format!(
                    "Server instructions truncated from {length} to {MAX_MCP_DESCRIPTION_LENGTH} chars"
                ),
            );
        }

        // One whose memo entry a `clearServerCache` took while it was being
        // made is closed at once. Its waiters still get it as connected, as
        // CC's do: CC's clear awaits the connect and then `cleanup()`s the
        // client they hold. Fetches through it come back empty, and an
        // automatic reconnect that made it stops, as in CC.
        if watch_connection(&client, service, memoized) {
            let subscribe = connected.capabilities.resources.is_some();
            crate::utils::debug::log_for_debugging(&format!(
                "[MCP] Server \"{name}\" connected with subscribe={subscribe}"
            ));
        }

        McpServerConnection::Connected(connected)
    }

    /// Maps to: CC `client.ts:1688-1704` `ensureConnectedClient`: the
    /// memoized connection for `client`'s name and config — the same one
    /// while it is healthy, a fresh one once its close or a clear deleted the
    /// memo entry. SDK servers run in-process and are returned as they are.
    pub async fn ensure_connected_client(
        client: &ConnectedMcpServer,
    ) -> anyhow::Result<ConnectedMcpServer> {
        if client.config.transport == Transport::Sdk {
            return Ok(client.clone());
        }
        match connect_to_server(&client.name, &client.config).await {
            McpServerConnection::Connected(connected) => Ok(connected),
            _ => anyhow::bail!("MCP server \"{}\" is not connected", client.name),
        }
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
                let mut connected_config = config.clone();
                connected_config.scope = crate::services::mcp::types::ConfigScope::Dynamic;
                let transport = SdkControlClientTransport::new(name.clone(), send_mcp_message);
                let handler = NyanMcpClientHandler::new(&name);
                match serve_client(handler, transport).await {
                    Ok(service) => {
                        let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
                        let client =
                            McpClient::new(connection_id, name.clone(), connected_config, &service);
                        // `client.ts:3296-3310`: capabilities only.
                        let connected = connected_mcp_server(client.clone());
                        let has_tools = connected.capabilities.tools.is_some();
                        watch_connection(&client, service, false);
                        let server = McpServerConnection::Connected(connected);
                        // CC `fetchToolsForClient(connectedClient)` when the
                        // server has tools (`client.ts:3312-3317`).
                        let tools = if has_tools {
                            fetch_tools_for_client(&server).await
                        } else {
                            Vec::new()
                        };
                        (server, tools)
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, server = %name, "failed to connect SDK MCP server");
                        let mut failed_config = config.clone();
                        failed_config.scope = crate::services::mcp::types::ConfigScope::User;
                        // CC `:3328-3333`: a failed SDK record carries no error.
                        let server = McpServerConnection::failed(name, failed_config, None);
                        (server, Vec::new())
                    }
                }
            }
        })).await;
        // CC `:3338-3343`: every client, and every client's tools.
        let (clients, tools): (Vec<_>, Vec<_>) = clients.into_iter().unzip();
        SdkMcpClientsSetup {
            clients,
            tools: tools.into_iter().flatten().collect(),
        }
    }

    /// Maps to: CC `client.ts:2137-2210` `reconnectMcpServerImpl`: clear the
    /// server's caches, connect, then fetch through the caches.
    pub async fn reconnect_mcp_server_impl(
        name: &str,
        config: &ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        // Re-read credentials another process may have changed.
        crate::utils::secure_storage::mac_os_keychain_helpers::clear_keychain_cache();
        clear_server_cache(name, Some(config)).await;
        let client = connect_to_server(name, config).await;
        let mut result = if client.connection_type() == McpServerConnectionType::Connected {
            fetch_connected_capabilities(client).await
        } else {
            McpConnectionDiscovery::from(client)
        };
        result.add_reconnect_resource_tools();
        result
    }

    /// The fetches CC's `processServer` (`client.ts:2344-2355`) and
    /// `reconnectMcpServerImpl` (`:2171-2178`) both make for a connected
    /// client: tools and prompts, and resources when it supports them.
    async fn fetch_connected_capabilities(client: McpServerConnection) -> McpConnectionDiscovery {
        let (tools, commands, resources) = tokio::join!(
            fetch_tools_for_client(&client),
            fetch_commands_for_client(&client),
            async {
                if supports_resources(&client) {
                    fetch_resources_for_client(&client).await
                } else {
                    Vec::new()
                }
            },
        );
        McpConnectionDiscovery {
            server: client,
            tools,
            commands,
            // `resources.length > 0 ? resources : undefined` (`:2197`, `:2370`).
            resources: (!resources.is_empty()).then_some(resources),
        }
    }

    /// Maps to: CC `client.ts:2286-2357`, `processServer` from the needs-auth
    /// gate through the fetches.
    async fn discover_mcp_connection_entry(
        name: String,
        config: ScopedMcpServerConfig,
    ) -> McpConnectionDiscovery {
        let missing_oauth_token =
            crate::services::mcp::auth::has_mcp_discovery_but_no_token(&name, &config);
        if (remote_transport_uses_auth_cache(config.transport) && is_mcp_auth_cached(&name))
            || missing_oauth_token
        {
            return McpConnectionDiscovery::needs_auth(name, &config);
        }
        let client = connect_to_server(&name, &config).await;
        match client.connection_type() {
            McpServerConnectionType::Connected => fetch_connected_capabilities(client).await,
            McpServerConnectionType::NeedsAuth => McpConnectionDiscovery::needs_auth(name, &config),
            _ => McpConnectionDiscovery::from(client),
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
        params.meta = meta.map(RequestMetaObject::from);
        params
    }

    /// A request on `client` failed: the connection's `onerror` sees the
    /// error first.
    fn request_failed(client: &McpClient, error: anyhow::Error) -> anyhow::Error {
        close_on_expired_session(client, &error);
        error
    }

    /// Maps to: CC `callMCPTool` (`client.ts:3029-3245`)'s request on the
    /// client it is given.
    async fn call_mcp_tool_once(
        client: &McpClient,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
    ) -> anyhow::Result<Value> {
        let timeout = Duration::from_millis(super::get_mcp_tool_timeout_ms_from_env(|key| {
            crate::utils::process_env::var(key)
        }));
        let params = call_tool_request_params(tool_name, args, meta);
        let call_result = tokio::time::timeout(timeout, client.peer()?.call_tool(params)).await;
        let result = match call_result {
            Err(_) => anyhow::bail!("MCP tool call timed out"),
            Ok(Ok(result)) => result,
            Ok(Err(error)) => return Err(request_failed(client, anyhow::Error::new(error))),
        };
        Ok(serde_json::to_value(result)?)
    }

    /// The connection to retry on after a remote auth failure, once a token
    /// refresh succeeded and a fresh connection was made with it.
    async fn handle_remote_call_auth_failure_and_retry_connect(
        server_name: &str,
        config: &ScopedMcpServerConfig,
        error: &anyhow::Error,
    ) -> anyhow::Result<Option<McpClient>> {
        record_remote_auth_challenge(server_name, config, error)?;
        if refresh_after_remote_auth_failure(server_name, config, error).await? {
            clear_server_cache(server_name, Some(config)).await;
            let client = connect_to_server(server_name, config).await;
            return Ok(client
                .as_connected()
                .map(|connected| connected.client.clone()));
        }
        set_mcp_auth_cache_entry(server_name);
        Ok(None)
    }

    /// Maps to: CC `client.ts:3224-3230`: the expired session's connection
    /// goes, so the next tool call creates a fresh session, then
    /// `McpSessionExpiredError`. CC clears by key — `clearServerCache(name,
    /// config)` — which also takes a connection another failed call has just
    /// made under that key through `ensureConnectedClient`; here only the
    /// failed call's own connection is closed.
    fn session_expired(client: &McpClient) -> super::McpSessionExpiredError {
        tracing::debug!(
            server = client.name(),
            "MCP session expired during tool call, clearing connection cache for re-initialization"
        );
        close_connection(client);
        super::McpSessionExpiredError::new(client.name())
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

    /// Maps to: CC `callMCPTool` (`client.ts:3029-3245`) on `client`: the
    /// request, then its error handling — an expired session clears the
    /// connection and throws `McpSessionExpiredError` (`:3210-3231`). A 401
    /// refreshes the token and retries once on a fresh connection, where CC
    /// throws `McpAuthError` (`:3198-3208`). The result is the raw
    /// `CallToolResult`; `isError` and `processMCPResult` are the caller's.
    pub async fn call_mcp_tool(
        client: &McpClient,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
    ) -> anyhow::Result<Value> {
        let server_name = client.name();
        let config = client.config();
        let error = match call_mcp_tool_once(client, tool_name, args.clone(), meta.clone()).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if is_auth_error(&error)
            && let Some(retry_client) =
                handle_remote_call_auth_failure_and_retry_connect(server_name, config, &error)
                    .await?
        {
            return match call_mcp_tool_once(&retry_client, tool_name, args, meta).await {
                Ok(value) => Ok(value),
                Err(retry_error) => {
                    record_final_remote_call_error(
                        server_name,
                        retry_client.config(),
                        &retry_error,
                    )?;
                    if is_tool_call_session_expired_error(&retry_error, retry_client.config()) {
                        return Err(session_expired(&retry_client).into());
                    }
                    Err(retry_error)
                }
            };
        }
        if is_tool_call_session_expired_error(&error, config) {
            return Err(session_expired(client).into());
        }
        Err(error)
    }

    /// Maps to: CC `callMCPToolWithUrlElicitationRetry` (`client.ts:2813`)
    /// around [`call_mcp_tool`] on `client`. For an MCP tool's `call` that is
    /// the connection `ensureConnectedClient` returned (`:1862-1881`);
    /// `handle_elicitation` is the print/SDK leg it passes from
    /// `context.handleElicitation`.
    pub async fn call_mcp_tool_with_elicitation(
        client: &McpClient,
        tool_name: &str,
        args: Map<String, Value>,
        meta: Option<Map<String, Value>>,
        handle_elicitation: crate::tool::HandleElicitationCallback,
    ) -> anyhow::Result<Value> {
        let mut url_elicitation_attempt = 0usize;
        loop {
            let error = match call_mcp_tool(client, tool_name, args.clone(), meta.clone()).await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            if !is_url_elicitation_required_error(&error)
                || url_elicitation_attempt >= MAX_URL_ELICITATION_RETRIES
            {
                return Err(error);
            }
            let elicitations = url_elicitations_from_error(&error);
            if elicitations.is_empty() {
                return Err(error);
            }
            if let Some(result) = process_url_elicitation_required(
                client.name(),
                tool_name,
                elicitations,
                handle_elicitation.clone(),
            )
            .await
            {
                return Ok(result);
            }
            url_elicitation_attempt += 1;
        }
    }

    /// Maps to: CC `callIdeRpc(toolName, args, client)` (`client.ts:2116-2128`):
    /// `callMCPTool` on the IDE record's client, returning the processed
    /// content. A tool result with `isError` throws (`:3123-3146`).
    pub async fn call_ide_rpc(
        tool_name: &str,
        args: Map<String, Value>,
        client: &ConnectedMcpServer,
    ) -> anyhow::Result<Value> {
        let result = call_mcp_tool(&client.client, tool_name, args, None).await?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            // A non-empty `content` decides, even when its first block has
            // no text; only without one does the legacy `error` field count.
            let error_details = match result
                .get("content")
                .and_then(Value::as_array)
                .filter(|content| !content.is_empty())
            {
                Some(content) => content[0].get("text").map(js_string),
                None => result.get("error").map(js_string),
            };
            anyhow::bail!(
                "{}",
                error_details.unwrap_or_else(|| "Unknown error".to_owned())
            );
        }
        Ok(super::process_mcp_result(&result, tool_name, &client.name)
            .await?
            .content)
    }

    async fn get_mcp_prompt_once(
        client: &McpClient,
        prompt_name: &str,
        arg_names: &[String],
        args: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let params = GetPromptRequestParams::new(prompt_name.to_string())
            .with_arguments(super::mcp_prompt_arguments_from_args(arg_names, args));
        let result = match client.peer()?.get_prompt(params).await {
            Ok(result) => result,
            Err(error) => return Err(request_failed(client, anyhow::Error::new(error))),
        };
        let mut blocks = Vec::new();
        for message in result.messages {
            let content = serde_json::to_value(message.content)?;
            blocks.extend(super::transform_result_content(&content, client.name())?);
        }
        Ok(blocks)
    }

    /// Maps to: CC MCP prompt command `getPromptForCommand(args)` from
    /// `fetchCommandsForClient(...)` (`client.ts:2073-2093`): the prompt is
    /// fetched over `ensureConnectedClient(client)`, `client` being the
    /// connection the command was fetched from.
    pub async fn get_mcp_prompt_for_command(
        client: &ConnectedMcpServer,
        prompt_name: &str,
        arg_names: &[String],
        args: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let connected = ensure_connected_client(client).await?;
        let server_name = connected.name.as_str();
        match get_mcp_prompt_once(&connected.client, prompt_name, arg_names, args).await {
            Ok(value) => Ok(value),
            Err(error) => {
                if is_auth_error(&error)
                    && let Some(retry_client) = handle_remote_call_auth_failure_and_retry_connect(
                        server_name,
                        &connected.config,
                        &error,
                    )
                    .await?
                {
                    get_mcp_prompt_once(&retry_client, prompt_name, arg_names, args).await
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn read_mcp_resource_once(client: &McpClient, uri: &str) -> anyhow::Result<Value> {
        let peer = client.peer()?;
        // Maps to: CC `ReadMcpResourceTool.call(...)` `client.capabilities?.resources`
        // guard before sending `resources/read`.
        if !peer
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some())
        {
            anyhow::bail!("Server \"{}\" does not support resources", client.name());
        }
        let result = match peer
            .read_resource(ReadResourceRequestParams::new(uri.to_string()))
            .await
        {
            Ok(result) => result,
            Err(error) => return Err(request_failed(client, anyhow::Error::new(error))),
        };
        Ok(serde_json::to_value(result)?)
    }

    /// Maps to: CC `resources/read` on `client` — the connection
    /// `ensureConnectedClient` returned (`ReadMcpResourceTool.ts:94-101`), or
    /// the AppState record's own client (`client.client.readResource`,
    /// `attachments.ts:2032`).
    pub async fn read_mcp_resource(client: &McpClient, uri: &str) -> anyhow::Result<Value> {
        let server_name = client.name();
        match read_mcp_resource_once(client, uri).await {
            Ok(value) => Ok(value),
            Err(error) => {
                if is_auth_error(&error)
                    && let Some(retry_client) = handle_remote_call_auth_failure_and_retry_connect(
                        server_name,
                        client.config(),
                        &error,
                    )
                    .await?
                {
                    return match read_mcp_resource_once(&retry_client, uri).await {
                        Ok(value) => Ok(value),
                        Err(retry_error) => {
                            record_final_remote_call_error(
                                server_name,
                                retry_client.config(),
                                &retry_error,
                            )?;
                            Err(retry_error)
                        }
                    };
                }
                Err(error)
            }
        }
    }
}

pub use runtime::{
    McpClient, call_ide_rpc, call_mcp_tool, call_mcp_tool_with_elicitation, cleanup_connection,
    cleanup_memoized_connection, clear_mcp_auth_cache, clear_server_cache, connect_to_server,
    ensure_connected_client, fetch_resources_for_client, fetch_tools_for_client,
    get_mcp_prompt_for_command, get_mcp_tools_commands_and_resources, read_mcp_resource,
    reconnect_mcp_server_impl, refresh_mcp_prompts_for_client, refresh_mcp_resources_for_client,
    refresh_mcp_tools_for_client, setup_sdk_mcp_clients,
};

/// A stdio MCP server for the connection and fetch caches. It appends each
/// `initialize` and `tools/list` it answers to the file named by its first
/// argument, followed for `initialize` by a `protocolVersion <requested>`
/// line, and answers the Nth `tools/list` it gets with one tool, `lookup_N`.
/// With `slow-first` as its second argument, the first `initialize` recorded
/// in that file is answered half a second late; with `exit-on-initialize`, it
/// exits instead of answering. Any tool call makes it exit, except one to
/// `complete`, which first sends `notifications/elicitation/complete` for
/// `elicit-1`. With `resources`, it declares resources and records each
/// `resources/read`: the first is answered with a one-minute `ttlMs` cache
/// hint, every later one with an error.
#[cfg(test)]
pub(crate) const COUNTING_STDIO_FIXTURE: &str = r#"
import fs from 'node:fs'
import readline from 'node:readline'
const counter = process.argv[2]
const slowFirst = process.argv[3] === 'slow-first'
const exitOnInitialize = process.argv[3] === 'exit-on-initialize'
const resources = process.argv[3] === 'resources'
const rl = readline.createInterface({ input: process.stdin })
let listCalls = 0
let resourceReads = 0
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
function recorded() {
  try { return fs.readFileSync(counter, 'utf8') } catch { return '' }
}
rl.on('line', line => {
  let message
  try { message = JSON.parse(line) } catch { return }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    const first = !recorded().includes('initialize')
    fs.appendFileSync(counter, `initialize\nprotocolVersion ${message.params?.protocolVersion}\n`)
    if (exitOnInitialize) process.exit(1)
    const reply = () => send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: resources ? { tools: {}, resources: {} } : { tools: {} },
        serverInfo: { name: 'memo-fixture', version: '1.0.0' }
      }
    })
    if (slowFirst && first) setTimeout(reply, 500)
    else reply()
  } else if (message.method === 'tools/list') {
    listCalls += 1
    fs.appendFileSync(counter, 'tools/list\n')
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        tools: [{
          name: `lookup_${listCalls}`,
          description: 'Look something up',
          inputSchema: { type: 'object', properties: {}, additionalProperties: false }
        }]
      }
    })
  } else if (message.method === 'tools/call' && message.params?.name === 'complete') {
    send({
      jsonrpc: '2.0',
      method: 'notifications/elicitation/complete',
      params: { elicitationId: 'elicit-1' }
    })
    send({ jsonrpc: '2.0', id: message.id, result: { content: [] } })
  } else if (message.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: message.id, result: { content: [] } })
    setTimeout(() => process.exit(0), 10)
  } else if (message.method === 'resources/read') {
    resourceReads += 1
    fs.appendFileSync(counter, 'resources/read\n')
    if (resourceReads === 1) {
      send({
        jsonrpc: '2.0',
        id: message.id,
        result: {
          contents: [{ uri: message.params.uri, text: 'fresh' }],
          ttlMs: 60000,
          cacheScope: 'public'
        }
      })
    } else {
      send({ jsonrpc: '2.0', id: message.id, error: { code: -32603, message: 'read failed' } })
    }
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#;

#[cfg(test)]
pub(crate) use runtime::memoized_connection_ids;

/// A streamable HTTP MCP server that gives every `initialize` a new session
/// (`s1`, `s2`, ...) and answers 404 to the requests `expires` picks.
#[cfg(test)]
pub(crate) mod session_http_fixture {
    use std::sync::{Arc, Mutex};

    pub(crate) struct SessionHttpFixture {
        pub(crate) url: String,
        /// `initialize s1`, `tools/call s1`, `prompts/get s2`, ...
        pub(crate) log: Arc<Mutex<Vec<String>>>,
        /// The `params` of each `tools/call` answered.
        pub(crate) tool_calls: Arc<Mutex<Vec<serde_json::Value>>>,
        /// `tools/call` results by tool name; any other tool answers
        /// `ok:<session>`.
        pub(crate) tool_results: Arc<Mutex<std::collections::HashMap<String, serde_json::Value>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for SessionHttpFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl SessionHttpFixture {
        /// `expires(method, session)` picks the requests answered with a 404
        /// whose body is `not_found_body`. Serves on the current runtime.
        pub(crate) async fn spawn(
            expires: impl Fn(&str, &str) -> bool + Send + 'static,
            not_found_body: &'static str,
        ) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind HTTP MCP fixture");
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let log = Arc::new(Mutex::new(Vec::new()));
            let tool_calls = Arc::new(Mutex::new(Vec::new()));
            let tool_results = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let task = tokio::spawn({
                let log = log.clone();
                let tool_calls = tool_calls.clone();
                let tool_results = tool_results.clone();
                async move {
                    let mut sessions = 0;
                    loop {
                        let Ok((mut socket, _)) = listener.accept().await else {
                            return;
                        };
                        let mut buffer = Vec::new();
                        let mut chunk = [0_u8; 4096];
                        let request = loop {
                            let read = socket.read(&mut chunk).await.unwrap_or(0);
                            buffer.extend_from_slice(&chunk[..read]);
                            let raw = String::from_utf8_lossy(&buffer).to_string();
                            let Some(header_end) = raw.find("\r\n\r\n") else {
                                if read == 0 {
                                    break raw;
                                }
                                continue;
                            };
                            let length = raw[..header_end]
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if read == 0 || buffer.len() >= header_end + 4 + length {
                                break raw;
                            }
                        };
                        let (head, body) = request.split_once("\r\n\r\n").unwrap_or((&request, ""));
                        let verb = head.split(' ').next().unwrap_or_default();
                        let session = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("mcp-session-id")
                                    .then(|| value.trim().to_string())
                            })
                            .unwrap_or_default();
                        let message: serde_json::Value =
                            serde_json::from_str(body).unwrap_or_default();
                        let method = message["method"].as_str().unwrap_or_default();
                        let id = message.get("id").cloned().unwrap_or_default();
                        let respond = |status: &str, extra: &str, body: String| {
                            format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        };
                        let result = |result: serde_json::Value| {
                            respond(
                                "200 OK",
                                "",
                                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
                                    .to_string(),
                            )
                        };
                        let call = verb == "POST" && matches!(method, "tools/call" | "prompts/get");
                        if call {
                            log.lock().unwrap().push(format!("{method} {session}"));
                        }
                        let wire = match (verb, method) {
                            _ if call && expires(method, &session) => {
                                respond("404 Not Found", "", not_found_body.to_string())
                            }
                            ("POST", "initialize") => {
                                sessions += 1;
                                log.lock().unwrap().push(format!("initialize s{sessions}"));
                                respond(
                                    "200 OK",
                                    &format!("Mcp-Session-Id: s{sessions}\r\n"),
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": {
                                            "protocolVersion": "2025-11-25",
                                            "capabilities": { "tools": {}, "prompts": {} },
                                            "serverInfo": { "name": "session-fixture", "version": "1.0.0" }
                                        }
                                    })
                                    .to_string(),
                                )
                            }
                            ("POST", "tools/list") => result(serde_json::json!({ "tools": [{
                                "name": "lookup",
                                "inputSchema": { "type": "object", "properties": {} }
                            }] })),
                            ("POST", "prompts/list") => result(serde_json::json!({ "prompts": [{
                                "name": "daily"
                            }] })),
                            ("POST", "tools/call") => {
                                let params = message["params"].clone();
                                tool_calls.lock().unwrap().push(params.clone());
                                let preset = params["name"]
                                    .as_str()
                                    .and_then(|name| tool_results.lock().unwrap().get(name).cloned());
                                result(preset.unwrap_or_else(|| {
                                    serde_json::json!({
                                        "content": [{ "type": "text", "text": format!("ok:{session}") }]
                                    })
                                }))
                            }
                            ("POST", "prompts/get") => result(serde_json::json!({
                                "messages": [{
                                    "role": "user",
                                    "content": { "type": "text", "text": format!("prompt:{session}") }
                                }]
                            })),
                            ("POST", _) => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                            ("DELETE", _) => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                            _ => "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                        };
                        let _ = socket.write_all(wire.as_bytes()).await;
                    }
                }
            });
            Self {
                url,
                log,
                tool_calls,
                tool_results,
                task,
            }
        }

        pub(crate) fn config(&self) -> super::ScopedMcpServerConfig {
            super::ScopedMcpServerConfig {
                name: None,
                scope: crate::services::mcp::types::ConfigScope::User,
                transport: super::Transport::Http,
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                url: Some(self.url.clone()),
                headers: std::collections::BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                plugin_source: None,
            }
        }

        pub(crate) fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        /// Answers `tools/call` for `tool` with `result` from now on.
        pub(crate) fn reply(&self, tool: &str, result: serde_json::Value) {
            self.tool_results
                .lock()
                .unwrap()
                .insert(tool.to_string(), result);
        }

        pub(crate) fn tool_calls(&self) -> Vec<serde_json::Value> {
            self.tool_calls.lock().unwrap().clone()
        }
    }

    /// The body MCP servers send with a session-not-found 404.
    pub(crate) const SESSION_NOT_FOUND: &str =
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32001,"message":"Session not found"}}"#;
}

/// Test helpers for [`COUNTING_STDIO_FIXTURE`].
#[cfg(test)]
pub(crate) mod counting_fixture {
    use super::*;

    /// A fixture under a fresh temp directory; removed on drop.
    pub(crate) struct CountingFixture {
        pub(crate) dir: std::path::PathBuf,
        pub(crate) script: std::path::PathBuf,
        pub(crate) log: std::path::PathBuf,
    }

    impl CountingFixture {
        pub(crate) fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("nyan-mcp-memo-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("server.mjs");
            std::fs::write(&script, COUNTING_STDIO_FIXTURE).expect("write MCP fixture");
            let log = dir.join("requests");
            Self { dir, script, log }
        }

        /// A stdio config running this fixture with `extra` arguments.
        pub(crate) fn config(&self, extra: &[&str]) -> ScopedMcpServerConfig {
            let mut args = vec![
                self.script.to_string_lossy().to_string(),
                self.log.to_string_lossy().to_string(),
            ];
            args.extend(extra.iter().map(|arg| arg.to_string()));
            ScopedMcpServerConfig {
                name: None,
                scope: crate::services::mcp::types::ConfigScope::User,
                transport: Transport::Stdio,
                command: Some("node".to_string()),
                args,
                env: std::collections::BTreeMap::new(),
                url: None,
                headers: std::collections::BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                plugin_source: None,
            }
        }

        /// How many `method` requests the fixture's processes have answered.
        pub(crate) fn count(&self, method: &str) -> usize {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .filter(|line| *line == method)
                .count()
        }

        /// Waits until `condition` holds; fails after five seconds.
        pub(crate) async fn until(&self, what: &str, mut condition: impl FnMut(&Self) -> bool) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !condition(self) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for {what}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }

    impl Drop for CountingFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use image::GenericImageView;
    use std::collections::BTreeMap;
    use std::io::{Read as _, Write as _};

    /// The connection a record holds, if it is connected.
    fn connection_id(server: &McpServerConnection) -> Option<u64> {
        server
            .as_connected()
            .map(|connected| connected.client.connection_id())
    }

    /// CC `serverInfo.version`, which only a connected record carries.
    fn server_version(server: &McpServerConnection) -> Option<&str> {
        server
            .as_connected()
            .and_then(|connected| connected.server_info.as_ref())
            .map(|info| info.version.as_str())
    }

    /// CC `error`, which only a failed record carries.
    fn failed_error(server: &McpServerConnection) -> Option<&str> {
        match server {
            McpServerConnection::Failed(failed) => failed.error.as_deref(),
            _ => None,
        }
    }

    /// A connected record for `name` whose server declares resources.
    fn connected_with_resources(name: &str) -> McpServerConnection {
        let mut server = McpServerConnection::connected_for_test(
            name,
            ScopedMcpServerConfig::stdio_for_test(name),
        );
        if let McpServerConnection::Connected(connected) = &mut server {
            connected.capabilities.resources = Some(Default::default());
        }
        server
    }

    /// A connected record for `name` with no connection behind it, as the
    /// client a fetch binds its tools to.
    fn connected_record(
        name: &str,
        config: ScopedMcpServerConfig,
    ) -> std::sync::Arc<ConnectedMcpServer> {
        std::sync::Arc::new(
            McpServerConnection::connected_for_test(name, config)
                .as_connected()
                .cloned()
                .expect("a connected record"),
        )
    }

    /// rmcp's Streamable HTTP default follows no redirect, so a server's
    /// custom headers never reach a redirect target; the proxy-aware client
    /// keeps that.
    #[tokio::test]
    async fn streamable_http_client_follows_no_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env: Vec<_> = [
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "HTTP_PROXY",
            "no_proxy",
        ]
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
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcp-contract-review-0915/oracle.json"
        ))
        .unwrap();
        let mut calls = Vec::new();
        for names in [vec!["a", "b"], vec!["c"]] {
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut completions = Vec::new();
            for name in names {
                let mut discovery = McpConnectionDiscovery::from(connected_with_resources(name));
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
            let mut discovery = McpConnectionDiscovery::from(connected_with_resources("reconnect"));
            let names = supplied
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            discovery.tools = supplied;
            discovery.add_reconnect_resource_tools();
            reconnects.push(serde_json::json!({"supplied": names, "returned": discovery.tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(), "resourcesUndefined": discovery.resources.is_none()}));
        }
        assert_eq!(serde_json::json!(reconnects), oracle["reconnectCases"]);
        let mut auth = McpConnectionDiscovery::from(McpServerConnection::needs_auth(
            "auth",
            ScopedMcpServerConfig::stdio_for_test("auth"),
        ));
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

    /// CC `useIdeSelection` (`useIdeSelection.ts:59-150`): the IDE client
    /// appearing resets the selection and gets a `selection_changed` handler
    /// that delivers to `onSelect`; a handler whose client is no longer the
    /// current one is ignored, and every change of client resets again.
    #[test]
    fn ide_selection_handler_follows_the_current_ide_client() {
        use crate::hooks::use_ide_selection::{
            IdeSelection, SELECTION_CHANGED_METHOD, ide_selection_reset, run_ide_selection_effect,
        };
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "nyan-mcp-ide-selection-{}.mjs",
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
  if (message.method === 'fixture/emit') {
    send({
      jsonrpc: '2.0',
      method: 'selection_changed',
      params: {
        selection: { start: { line: 1, character: 0 }, end: { line: 2, character: 3 } },
        text: 'ab',
        filePath: '/repo/a.rs'
      }
    })
    return
  }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: {},
        serverInfo: { name: 'ide-fixture', version: '1.0.0' }
      }
    })
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#,
        )
        .expect("write MCP fixture");

        block_on_with_process_runtime(async {
            let mut config = ScopedMcpServerConfig::stdio_for_test("node");
            config.args = vec![script_path.to_string_lossy().to_string()];
            let ide = runtime::connect_to_server("ide", &config).await;
            let ide = ide.as_connected().expect("a connected IDE").client.clone();
            let selection = IdeSelection {
                file_path: Some("/repo/a.rs".to_string()),
                text: Some("ab".to_string()),
                line_count: 2,
                line_start: Some(1),
            };
            let params = serde_json::json!({
                "selection": { "start": { "line": 1, "character": 0 }, "end": { "line": 2, "character": 3 } },
                "text": "ab",
                "filePath": "/repo/a.rs"
            });
            let current = Default::default();
            let (on_select, selected) = async_channel::unbounded();

            // No IDE, then none again: no change.
            run_ide_selection_effect(&current, None, &on_select);
            assert!(selected.try_recv().is_err());

            // The IDE appears: reset, and its handler delivers.
            run_ide_selection_effect(&current, Some(ide.clone()), &on_select);
            assert_eq!(selected.try_recv().unwrap(), ide_selection_reset());
            ide.notification("fixture/emit", serde_json::json!({}))
                .await
                .expect("the fixture is told to notify");
            let delivered =
                tokio::time::timeout(std::time::Duration::from_secs(5), selected.recv())
                    .await
                    .expect("the selection is delivered")
                    .unwrap();
            assert_eq!(delivered, selection);

            // The IDE leaves: reset, and its handler is ignored from now on.
            run_ide_selection_effect(&current, None, &on_select);
            assert_eq!(selected.try_recv().unwrap(), ide_selection_reset());
            let first_handler = ide
                .notification_handler(SELECTION_CHANGED_METHOD)
                .expect("the handler stays registered");
            first_handler(Some(params.clone())).await;
            assert!(selected.try_recv().is_err());

            // Another IDE: reset; its handler delivers, the first's does not.
            let other = McpClient::detached_for_test("ide", &config);
            run_ide_selection_effect(&current, Some(other.clone()), &on_select);
            assert_eq!(selected.try_recv().unwrap(), ide_selection_reset());
            first_handler(Some(params.clone())).await;
            assert!(selected.try_recv().is_err());
            other
                .notification_handler(SELECTION_CHANGED_METHOD)
                .expect("the new IDE's handler")(Some(params))
            .await;
            assert_eq!(selected.try_recv().unwrap(), selection);

            runtime::clear_server_cache("ide", None).await;
        });
        let _ = std::fs::remove_file(script_path);
    }

    /// SDK `setNotificationHandler` (`shared/protocol.js:913-919`) keeps one
    /// handler per method, the later replacing the earlier: a remounted
    /// REPL's effect registers on the same IDE client, and only its channel
    /// receives the selections.
    #[test]
    fn a_later_notification_handler_replaces_the_earlier() {
        use crate::hooks::use_ide_selection::{
            SELECTION_CHANGED_METHOD, ide_selection_reset, run_ide_selection_effect,
        };
        let ide =
            McpClient::detached_for_test("ide", &ScopedMcpServerConfig::stdio_for_test("ide"));
        let params = serde_json::json!({
            "selection": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 2 } },
            "text": "ab"
        });

        let (first_select, first_selected) = async_channel::unbounded();
        run_ide_selection_effect(&Default::default(), Some(ide.clone()), &first_select);
        let (second_select, second_selected) = async_channel::unbounded();
        run_ide_selection_effect(&Default::default(), Some(ide.clone()), &second_select);
        assert_eq!(first_selected.try_recv().unwrap(), ide_selection_reset());
        assert_eq!(second_selected.try_recv().unwrap(), ide_selection_reset());

        let handler = ide
            .notification_handler(SELECTION_CHANGED_METHOD)
            .expect("a handler");
        futures::executor::block_on(handler(Some(params)));
        assert!(first_selected.try_recv().is_err());
        assert_eq!(second_selected.try_recv().unwrap().line_count, 1);
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
                if expected_length.is_none()
                    && let Some(header_end) =
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

    /// `AppState.mcp` after each update went through the connection flush.
    fn state_with_clients<T>(updates: Vec<T>) -> McpState
    where
        T: Into<crate::services::mcp::use_manage_mcp_connections::McpPendingUpdate>,
    {
        let mut state = McpState::default();
        for update in updates {
            crate::services::mcp::use_manage_mcp_connections::apply_mcp_server_update(
                &mut state, update,
            );
        }
        state
    }

    /// A connected server's connection attempt with what was fetched from it.
    fn connected_attempt(
        name: &str,
        tools: Vec<crate::types::tools::Tool>,
        commands: Vec<crate::commands::Command>,
    ) -> McpConnectionDiscovery {
        let mut attempt = McpConnectionDiscovery::from(McpServerConnection::connected_for_test(
            name,
            ScopedMcpServerConfig::stdio_for_test(name),
        ));
        attempt.tools = tools;
        attempt.commands = commands;
        attempt
    }

    /// Where the `mcp.tools` entry named `full_tool_name` sends its call: the
    /// server of the client it closes over and the server's own tool name
    /// (`mcpInfo.toolName`).
    fn mcp_tool_call_target(full_tool_name: &str, state: &McpState) -> Option<(String, String)> {
        let tool = state
            .tools
            .iter()
            .find(|tool| tool.is_mcp && tool.name == full_tool_name)?;
        let Some(crate::types::tools::McpToolConnection::Connected(client)) = &tool.mcp_connection
        else {
            return None;
        };
        Some((
            client.name.clone(),
            tool.mcp_info.as_ref()?.tool_name.clone(),
        ))
    }

    /// The client a fetched MCP tool's call closes over.
    fn bound_client(tool: &crate::types::tools::Tool) -> std::sync::Arc<ConnectedMcpServer> {
        match &tool.mcp_connection {
            Some(crate::types::tools::McpToolConnection::Connected(client)) => client.clone(),
            other => panic!("a fetched tool carries its client: {other:?}"),
        }
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
            std::env::temp_dir().join(format!("nyan-mcp-large-output-{}", uuid::Uuid::new_v4()));
        let _projects_guard = crate::utils::session_storage::set_test_projects_dir_override(&root);
        crate::bootstrap::state::set_original_cwd("/tmp/nyan-project");
        crate::bootstrap::state::set_session_id("session-mcp-large-output-test");
        let (endpoint, server) = spawn_count_tokens_server(r#"{"input_tokens":999}"#);
        let _config_home = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        let _max_tokens = EnvVarGuard::set("MAX_MCP_OUTPUT_TOKENS", "5");
        let _large_output = EnvVarGuard::set("ENABLE_MCP_LARGE_OUTPUT_FILES", "1");
        let _count_key = EnvVarGuard::set("NYAN_MCP_TEST_KEY", "sk-count-tokens");
        crate::utils::settings::settings_cache::reset_settings_cache();
        crate::services::provider::anthropic::target::install_test_main_profile(
            &endpoint,
            "NYAN_MCP_TEST_KEY",
        );
        struct ResetSettings;
        impl Drop for ResetSettings {
            fn drop(&mut self) {
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _reset_settings = ResetSettings;
        crate::services::provider::anthropic::target::install_test_model_profile(
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
        // `zipObject(argNames, args.split(' '))`: a name without a word gets
        // `undefined`, which the request's JSON leaves out. `''.split(' ')`
        // is `['']`, so the first name still gets an empty word.
        assert_eq!(
            mcp_prompt_arguments_from_args(&arg_names, "")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![("channel".to_string(), Value::String(String::new()))]
        );
        assert_eq!(
            mcp_prompt_arguments_from_args(&arg_names, "eng")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![("channel".to_string(), Value::String("eng".to_string()))]
        );
    }

    #[test]
    fn connection_update_writes_flat_mcp_commands_for_connected_clients() {
        let state = state_with_clients(vec![
            connected_attempt(
                "docs",
                Vec::new(),
                vec![mcp_command_for_test(
                    "docs",
                    "summarize",
                    Some("Summarize docs"),
                    &["path"],
                )],
            ),
            McpConnectionDiscovery::pending(
                "pending",
                &ScopedMcpServerConfig::stdio_for_test("pending"),
            ),
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
        let command = find_mcp_prompt_command("mcp__docs__summarize", &state.commands)
            .expect("the prompt is found");
        // CC `findCommand` also matches `getCommandName`, the user-facing
        // `server:prompt (MCP)`.
        assert_eq!(
            find_mcp_prompt_command("docs:summarize (MCP)", &state.commands),
            Some(command)
        );
        let binding = command
            .mcp_prompt
            .clone()
            .expect("the prompt carries its client");
        assert_eq!(binding.connection.name, "docs");
        assert_eq!(binding.prompt_name, "summarize");
        assert_eq!(command.arg_names, vec!["path"]);
        assert_eq!(
            crate::commands::get_command_name(command),
            "docs:summarize (MCP)"
        );

        // A reconnecting server keeps its commands (CC's flush only replaces
        // a dimension the update supplies), and the command still carries the
        // client it was fetched from, though the server's record is now
        // pending.
        let mut state = state;
        crate::services::mcp::use_manage_mcp_connections::apply_mcp_client_update(
            &mut state,
            McpConnectionDiscovery::pending("docs", &ScopedMcpServerConfig::stdio_for_test("docs"))
                .server,
        );
        assert_eq!(state.commands.len(), 1);
        assert_eq!(
            state
                .clients
                .iter()
                .find(|server| server.name() == "docs")
                .map(McpServerConnection::connection_type),
            Some(McpServerConnectionType::Pending)
        );
        let command = find_mcp_prompt_command("mcp__docs__summarize", &state.commands)
            .expect("the prompt is still found while the server reconnects");
        assert_eq!(command.mcp_prompt.as_ref(), Some(&binding));
    }

    #[test]
    fn prompt_commands_resolve_through_their_binding_not_their_name() {
        // CC's prompt command closes over its client and `prompt.name`, so
        // nothing is parsed back out of `mcp__<server>__<prompt>`: a server
        // name containing `__` or one that normalization rewrites still
        // resolves to its own server and prompt.
        let state = state_with_clients(vec![
            connected_attempt(
                "a__b",
                Vec::new(),
                vec![mcp_command_for_test("a__b", "x__y", None, &[])],
            ),
            connected_attempt(
                "My Server",
                Vec::new(),
                vec![mcp_command_for_test("My Server", "report", None, &[])],
            ),
        ]);

        let binding = find_mcp_prompt_command("mcp__a__b__x__y", &state.commands)
            .and_then(|command| command.mcp_prompt.as_ref())
            .expect("server a__b's prompt is found");
        assert_eq!(binding.connection.name, "a__b");
        assert_eq!(binding.prompt_name, "x__y");
        let binding = find_mcp_prompt_command("mcp__My_Server__report", &state.commands)
            .and_then(|command| command.mcp_prompt.as_ref())
            .expect("the normalized server's prompt is found");
        assert_eq!(binding.connection.name, "My Server");
        assert_eq!(binding.prompt_name, "report");
    }

    /// The bindings are the closed-over objects: equal only when they are
    /// the same capture, as JS compares objects by reference.
    #[test]
    fn mcp_bindings_compare_by_identity() {
        let first = connected_record_for_test("docs");
        let same_contents = std::sync::Arc::new((*first).clone());
        assert_eq!(*first, *same_contents, "the records themselves are equal");

        let tool = |client: &std::sync::Arc<ConnectedMcpServer>| {
            mcp_tool_from_server_tool(
                client,
                false,
                serde_json::from_value(serde_json::json!({
                    "name": "search",
                    "inputSchema": {"type": "object"}
                }))
                .unwrap(),
            )
        };
        assert_eq!(tool(&first).mcp_connection, tool(&first).mcp_connection);
        assert_ne!(
            tool(&first).mcp_connection,
            tool(&same_contents).mcp_connection
        );

        let command =
            |client| crate::commands::Command::from_mcp_prompt(client, "p", None, Vec::new());
        assert_eq!(command(&first).mcp_prompt, command(&first).mcp_prompt);
        assert_ne!(
            command(&first).mcp_prompt,
            command(&same_contents).mcp_prompt
        );
    }

    /// CC `callIdeRpc` (`client.ts:2116-2128`) returns the processed content
    /// of `callMCPTool`, which throws on `isError`: the first content block's
    /// text, or "Unknown error" when that block has none (`:3123-3146`).
    /// `closeOpenDiffs` (`utils/ide.ts:1266-1278`) sends `closeAllDiffTabs`
    /// and ignores a failure.
    #[test]
    fn ide_rpc_returns_the_content_and_throws_tool_errors() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        block_on_with_process_runtime(async {
            use session_http_fixture::{SESSION_NOT_FOUND, SessionHttpFixture};
            let fixture = SessionHttpFixture::spawn(|_, _| false, SESSION_NOT_FOUND).await;
            fixture.reply(
                "fine",
                serde_json::json!({ "content": [{ "type": "text", "text": "done" }] }),
            );
            fixture.reply(
                "text",
                serde_json::json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": "boom" }],
                }),
            );
            fixture.reply(
                "no-text",
                serde_json::json!({
                    "isError": true,
                    "content": [{ "type": "image", "data": "", "mimeType": "image/png" }],
                }),
            );
            fixture.reply(
                "closeAllDiffTabs",
                serde_json::json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": "unsupported" }],
                }),
            );
            let ide = runtime::connect_to_server("ide", &fixture.config()).await;
            let ide = ide.as_connected().expect("a connected IDE record");

            let content = runtime::call_ide_rpc("fine", Map::new(), ide)
                .await
                .expect("content");
            assert_eq!(content[0]["text"], "done");
            let error = |tool: &'static str| async move {
                runtime::call_ide_rpc(tool, Map::new(), ide)
                    .await
                    .expect_err("a tool error")
                    .to_string()
            };
            assert_eq!(error("text").await, "boom");
            assert_eq!(error("no-text").await, "Unknown error");

            crate::utils::ide::close_open_diffs(ide).await;
            let last = fixture.tool_calls().pop().expect("a tools/call");
            assert_eq!(last["name"], "closeAllDiffTabs");
            assert_eq!(last["arguments"], serde_json::json!({}));

            runtime::clear_server_cache("ide", None).await;
        });
    }

    /// A request on a handle with no connection behind it answers "not
    /// connected".
    #[test]
    fn requests_on_a_handle_without_a_connection_are_not_connected() {
        let config = ScopedMcpServerConfig::stdio_for_test("docs");
        let client = McpClient::detached_for_test("docs", &config);
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(call_mcp_tool_with_elicitation(
                &client,
                "search",
                Map::new(),
                None,
                crate::tool::HandleElicitationCallback::default(),
            ))
            .expect_err("no connection");
        assert_eq!(error.to_string(), "MCP server \"docs\" is not connected");
    }

    #[test]
    fn resource_capability_injects_helpers_even_when_resource_list_is_empty() {
        let mut discovery =
            McpConnectionDiscovery::from(connected_with_resources("empty-resources"));
        discovery.add_discovery_resource_tools(&std::sync::atomic::AtomicBool::new(false));
        let state = state_with_clients(vec![discovery]);
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
            connected_attempt(
                "GitHub Server",
                vec![mcp_tool_for_test(
                    "GitHub Server",
                    serde_json::json!({
                        "name": "Create Issue",
                        "description": "Create a GitHub issue",
                        "inputSchema": {"type": "object"},
                        "annotations": {"title": "Create Issue", "openWorldHint": true}
                    }),
                )],
                Vec::new(),
            ),
            McpConnectionDiscovery::pending(
                "pending",
                &ScopedMcpServerConfig::stdio_for_test("pending"),
            ),
        ]);

        let tools = &state.tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__GitHub_Server__Create_Issue");
        assert!(tools[0].is_mcp);
        assert_eq!(tools[0].description, "Create a GitHub issue");
        assert!(has_pending_mcp_servers(&state));
        assert_eq!(
            mcp_tool_call_target("mcp__GitHub_Server__Create_Issue", &state),
            Some(("GitHub Server".to_string(), "Create Issue".to_string()))
        );
    }

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
        assert_eq!(setup.clients[0].name(), "sdk-server");
        assert_eq!(
            setup.clients[0].connection_type(),
            McpServerConnectionType::Failed
        );
        // CC `client.ts:3328-3333`: the failed SDK record is `{ type:
        // 'failed', name, config }` — the error is only logged, never stored.
        assert!(matches!(
            &setup.clients[0],
            McpServerConnection::Failed(failed) if failed.error.is_none()
        ));
        assert_eq!(
            setup.clients[0].config().scope,
            crate::services::mcp::types::ConfigScope::User
        );
        assert!(setup.tools.is_empty());
        assert_eq!(seen_servers.lock().unwrap().as_slice(), &["sdk-server"]);
    }

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
                                "serverInfo": { "name": "sdk-connected", "version": "1.2.3" },
                                "instructions": "Use the echo tool."
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
        assert_eq!(client.name(), "sdk-connected");
        assert_eq!(client.connection_type(), McpServerConnectionType::Connected);
        let connected = client.as_connected().expect("a connected SDK client");
        // CC's SDK record (`client.ts:3301-3310`) sets neither `serverInfo`
        // nor `instructions`, though the server sent both during `initialize`.
        assert_eq!(connected.server_info, None);
        assert_eq!(connected.instructions, None);
        assert_eq!(
            client.config().scope,
            crate::services::mcp::types::ConfigScope::Dynamic
        );
        // CC `ensureConnectedClient` returns an SDK client as it is
        // (`client.ts:1692-1694`).
        assert_eq!(
            Some(
                ensure_connected_client(connected)
                    .await
                    .expect("an SDK client")
                    .client
                    .connection_id()
            ),
            connection_id(client)
        );
        assert_eq!(setup.tools.len(), 1);
        assert_eq!(setup.tools[0].name, "mcp__sdk-connected__echo");
        let methods = methods.lock().unwrap().clone();
        assert!(methods.contains(&"initialize".to_string()));
        assert!(methods.contains(&"tools/list".to_string()));
        // CC setupSdkMcpClients only discovers tools, even if resources exist.
        assert!(connected.capabilities.resources.is_some());
        assert!(
            !methods
                .iter()
                .any(|method| method == "prompts/list" || method == "resources/list")
        );
        // CC keeps every client it makes: setting the server up again makes
        // another beside it.
        let replacement = setup_sdk_mcp_clients(&configs, callback.clone()).await;
        let replacement_client = &replacement.clients[0]
            .as_connected()
            .expect("the replacement SDK client is connected")
            .client;
        assert_ne!(&connected.client, replacement_client);
        // `clearServerCache` never reaches an SDK server's `cleanup()`: CC's
        // `connectToServer` rejects SDK configs (`client.ts:866-867`).
        clear_server_cache("sdk-connected", Some(replacement.clients[0].config())).await;
        clear_server_cache("sdk-connected", None).await;
        assert!(!connected.client.is_closed());
        assert!(!replacement_client.is_closed());
        cleanup_connection(&connected.client);
        cleanup_connection(replacement_client);
        assert!(connected.client.is_closed());
        assert!(replacement_client.is_closed());
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
            plugin_source: None,
        };
        // One `tools/list` entry, mapped as `fetchToolsForClient` maps it.
        let client = connected_record("sdk server", sdk_config);
        let fetched = || {
            mcp_tools_from_server_tools(
                &client,
                vec![
                    serde_json::from_value(serde_json::json!({
                        "name": "Read",
                        "description": "SDK read",
                        "inputSchema": {"type": "object"},
                        "annotations": {"readOnlyHint": true}
                    }))
                    .unwrap(),
                ],
            )
        };
        let state_with_tools = |tools| McpState {
            tools,
            ..McpState::default()
        };

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let key = "NYAN_SDK_MCP_NO_PREFIX";
        let _prefix = EnvVarGuard::unset(key);

        // Maps to: CC `client.ts:1774` — `mcpInfo` holds the unnormalized server
        // and tool names and is set regardless of `skipPrefix`.
        let expected_mcp_info = Some(crate::types::tools::McpToolInfo {
            server_name: "sdk server".to_string(),
            tool_name: "Read".to_string(),
        });

        let prefixed = fetched();
        let state = state_with_tools(prefixed.clone());
        assert_eq!(prefixed[0].name, "mcp__sdk_server__Read");
        assert_eq!(prefixed[0].mcp_info, expected_mcp_info);
        assert_eq!(
            mcp_tool_call_target("mcp__sdk_server__Read", &state),
            Some(("sdk server".to_string(), "Read".to_string()))
        );

        crate::utils::process_env::set(key, "1");
        let unprefixed = fetched();
        let state = state_with_tools(unprefixed.clone());
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
            mcp_tool_call_target("Read", &state),
            Some(("sdk server".to_string(), "Read".to_string()))
        );
    }

    #[test]
    fn ide_mcp_tools_are_filtered_like_official_fetch_tools_for_client() {
        let config = ScopedMcpServerConfig {
            name: None,
            scope: crate::services::mcp::types::ConfigScope::Dynamic,
            transport: Transport::SseIde,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some("http://127.0.0.1:1/sse".to_string()),
            headers: BTreeMap::new(),
            headers_helper: None,
            oauth: None,
            ide_running_in_windows: None,
            ide_name: Some("IDE".to_string()),
            auth_token: None,
            plugin_source: None,
        };
        let tool = |name: &str| {
            serde_json::from_value::<rmcp::model::Tool>(
                serde_json::json!({"name": name, "inputSchema": {"type": "object"}}),
            )
            .unwrap()
        };
        let state = McpState {
            tools: mcp_tools_from_server_tools(
                &connected_record("ide", config),
                vec![
                    tool("executeCode"),
                    tool("getDiagnostics"),
                    tool("openFile"),
                ],
            ),
            ..McpState::default()
        };

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
            mcp_tool_call_target("mcp__ide__executeCode", &state),
            Some(("ide".to_string(), "executeCode".to_string()))
        );
        assert_eq!(mcp_tool_call_target("mcp__ide__openFile", &state), None);
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
            plugin_source: None,
        };
        let discovery = McpConnectionDiscovery::needs_auth("docs", &config);
        let state = state_with_clients(vec![discovery]);

        let tools = &state.tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "mcp__docs__authenticate");
        assert!(tools[0].is_mcp);
        assert!(tools[0].description.contains("requires authentication"));
        assert_eq!(
            tools[0]
                .mcp_info
                .as_ref()
                .map(|info| (info.server_name.as_str(), info.tool_name.as_str())),
            Some(("docs", "authenticate"))
        );
        // `createMcpAuthTool(serverName, config)` closes over the server's
        // name and config, not a connection.
        assert_eq!(
            tools[0].mcp_connection,
            Some(crate::types::tools::McpToolConnection::Authenticate {
                server_name: "docs".to_string(),
                config,
            })
        );
    }

    #[test]
    fn url_elicitation_required_error_parsing_matches_official_validation() {
        let error = anyhow::Error::new(rmcp::ErrorData::new(
            runtime::URL_ELICITATION_REQUIRED,
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

    /// A connected record with no connection behind it, its config from
    /// `plugin_source`, declaring the experimental `capabilities`.
    fn channel_server(
        name: &str,
        plugin_source: Option<&str>,
        capabilities: &[&str],
    ) -> McpServerConnection {
        let mut config = ScopedMcpServerConfig::stdio_for_test(name);
        config.plugin_source = plugin_source.map(str::to_owned);
        let mut server = McpServerConnection::connected_for_test(name, config);
        if let McpServerConnection::Connected(connected) = &mut server {
            connected.capabilities.experimental = Some(
                capabilities
                    .iter()
                    .map(|capability| (capability.to_string(), Map::new()))
                    .collect(),
            );
        }
        server
    }

    /// The channel gate's inputs, everything open but the session's
    /// `--channels` list, which is `channels`.
    fn open_channel_gate(
        channels: Vec<crate::bootstrap::state::ChannelEntry>,
    ) -> crate::services::mcp::channel_notification::ChannelRuntimeGateContext {
        use crate::services::mcp::channel_notification::{
            ChannelRuntimeGateContext, EffectiveChannelAllowlist,
        };
        ChannelRuntimeGateContext {
            channels_enabled: true,
            session_channels: channels,
            allowlist: EffectiveChannelAllowlist {
                entries: Vec::new(),
            },
        }
    }

    /// CC's channel block (`useManageMCPConnections.ts:470-614`): the gate
    /// decides when the client connects whether it gets a channel handler,
    /// which queues each message (`:507-531`); a later connect the gate skips
    /// removes it (`:562-572`).
    #[test]
    fn channel_messages_are_queued_only_on_a_client_the_gate_registered() {
        use crate::bootstrap::state::ChannelEntry;
        use crate::services::mcp::channel_notification::{
            CHANNEL_EXPERIMENTAL_CAPABILITY, CHANNEL_MESSAGE_NOTIFICATION_METHOD,
        };
        use crate::services::mcp::use_manage_mcp_connections::install_channel_handlers;
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::utils::message_queue_manager::clear_command_queue();
        let listed = open_channel_gate(vec![ChannelEntry::server("planner", true)]);

        let server = channel_server("planner", None, &[CHANNEL_EXPERIMENTAL_CAPABILITY]);
        let connected = server.as_connected().expect("connected");
        install_channel_handlers(connected, &listed, None);
        let handler = connected
            .client
            .notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD)
            .expect("a channel handler");
        futures::executor::block_on(handler(Some(serde_json::json!({
            "content": "hello",
            "meta": { "thread_ts": "1" }
        }))));
        let queue = crate::utils::message_queue_manager::get_command_queue();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].mode, "prompt");
        assert!(queue[0].is_meta);
        assert!(queue[0].skip_slash_commands);
        assert!(
            queue[0]
                .value
                .contains("<channel source=\"planner\" thread_ts=\"1\">")
        );
        crate::utils::message_queue_manager::clear_command_queue();

        // Connected again while not in the session's `--channels` list.
        install_channel_handlers(connected, &open_channel_gate(Vec::new()), None);
        assert!(
            connected
                .client
                .notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD)
                .is_none()
        );

        // A server that never declared the capability gets none.
        let plain = channel_server("planner", None, &[]);
        let plain = plain.as_connected().expect("connected");
        install_channel_handlers(plain, &listed, None);
        assert!(
            plain
                .client
                .notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD)
                .is_none()
        );
    }

    /// CC registers the permission-reply handler beside a registered channel
    /// handler, for a server that declared `claude/channel/permission`
    /// (`useManageMCPConnections.ts:533-560`): a reply resolves the session's
    /// pending relay, and a stale one resolves nothing. A skip removes it.
    #[test]
    fn channel_permission_replies_resolve_the_session_relays() {
        use crate::bootstrap::state::ChannelEntry;
        use crate::services::mcp::channel_notification::{
            CHANNEL_EXPERIMENTAL_CAPABILITY, CHANNEL_MESSAGE_NOTIFICATION_METHOD,
            CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY, CHANNEL_PERMISSION_METHOD,
        };
        use crate::services::mcp::channel_permissions::{
            ChannelPermissionBehavior, ChannelPermissionCallbacks,
        };
        use crate::services::mcp::use_manage_mcp_connections::install_channel_handlers;
        let callbacks = ChannelPermissionCallbacks::default();
        let (tx, rx) = async_channel::bounded(1);
        let _unsubscribe = callbacks.on_response("abcde", move |response| {
            tx.try_send(response).unwrap();
        });
        let listed = open_channel_gate(vec![ChannelEntry::server("telegram", true)]);

        let server = channel_server(
            "telegram",
            None,
            &[
                CHANNEL_EXPERIMENTAL_CAPABILITY,
                CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY,
            ],
        );
        let connected = server.as_connected().expect("connected");
        install_channel_handlers(connected, &listed, Some(callbacks.clone()));
        let reply = connected
            .client
            .notification_handler(CHANNEL_PERMISSION_METHOD)
            .expect("a permission handler");
        futures::executor::block_on(reply(Some(serde_json::json!({
            "request_id": "ABCDE",
            "behavior": "allow"
        }))));
        let response = rx.try_recv().unwrap();
        assert_eq!(response.behavior, ChannelPermissionBehavior::Allow);
        assert_eq!(response.from_server, "telegram");
        futures::executor::block_on(reply(Some(serde_json::json!({
            "request_id": "abcde",
            "behavior": "deny"
        }))));
        assert!(rx.try_recv().is_err());

        // Connected again while not in the session's `--channels` list: both
        // handlers go (`:562-573`).
        let unlisted_gate = open_channel_gate(Vec::new());
        install_channel_handlers(connected, &unlisted_gate, Some(callbacks.clone()));
        assert!(
            connected
                .client
                .notification_handler(CHANNEL_PERMISSION_METHOD)
                .is_none()
        );

        // The capability alone is not enough: the reply handler comes only
        // with a registered channel.
        let unlisted = channel_server(
            "telegram",
            None,
            &[
                CHANNEL_EXPERIMENTAL_CAPABILITY,
                CHANNEL_PERMISSION_EXPERIMENTAL_CAPABILITY,
            ],
        );
        let unlisted = unlisted.as_connected().expect("connected");
        install_channel_handlers(unlisted, &unlisted_gate, Some(callbacks.clone()));
        assert!(
            unlisted
                .client
                .notification_handler(CHANNEL_PERMISSION_METHOD)
                .is_none()
        );

        let without = channel_server("telegram", None, &[CHANNEL_EXPERIMENTAL_CAPABILITY]);
        let without = without.as_connected().expect("connected");
        install_channel_handlers(without, &listed, Some(callbacks));
        assert!(
            without
                .client
                .notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD)
                .is_some()
        );
        assert!(
            without
                .client
                .notification_handler(CHANNEL_PERMISSION_METHOD)
                .is_none()
        );
    }

    /// The gate reads the connecting client's own `config.pluginSource`
    /// (`useManageMCPConnections.ts:473-478`): of two connections under one
    /// name asked for as `plugin:telegram@official`, only the one installed
    /// from that marketplace gets a channel handler.
    #[test]
    fn the_channel_gate_reads_the_connections_own_plugin_source() {
        use crate::bootstrap::state::ChannelEntry;
        use crate::services::mcp::channel_notification::{
            CHANNEL_EXPERIMENTAL_CAPABILITY, CHANNEL_MESSAGE_NOTIFICATION_METHOD,
        };
        use crate::services::mcp::use_manage_mcp_connections::install_channel_handlers;
        const SERVER: &str = "plugin:telegram:telegram";
        let listed = open_channel_gate(vec![ChannelEntry::plugin("telegram", "official", true)]);
        let official = channel_server(
            SERVER,
            Some("telegram@official"),
            &[CHANNEL_EXPERIMENTAL_CAPABILITY],
        );
        let elsewhere = channel_server(
            SERVER,
            Some("telegram@elsewhere"),
            &[CHANNEL_EXPERIMENTAL_CAPABILITY],
        );
        for server in [&official, &elsewhere] {
            install_channel_handlers(server.as_connected().expect("connected"), &listed, None);
        }
        let has_handler = |server: &McpServerConnection| {
            server
                .as_connected()
                .expect("connected")
                .client
                .notification_handler(CHANNEL_MESSAGE_NOTIFICATION_METHOD)
                .is_some()
        };
        assert!(has_handler(&official));
        assert!(!has_handler(&elsewhere));
    }

    /// Maps to: CC `callMCPToolWithUrlElicitationRetry` client.ts:2944-2947 —
    /// with a handleElicitation leg (print/SDK mode) the resolution delegates
    /// to it instead of queuing an ElicitationRequestEvent for the dialog.
    #[test]
    fn url_elicitation_prefers_the_sdk_handler_over_the_queue() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // Elicitation notification hooks are detached onto it.
        crate::utils::process_runtime::initialize_test_process_runtime();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = drain_mcp_connection_callback_observations().await;
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
                let events = drain_mcp_connection_callback_observations().await;
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

    #[test]
    fn url_elicitation_required_retry_queue_accept_and_decline_paths() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // Elicitation notification hooks are detached onto it.
        crate::utils::process_runtime::initialize_test_process_runtime();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = drain_mcp_connection_callback_observations().await;

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
                let events = drain_mcp_connection_callback_observations().await;
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
                    crate::services::mcp::elicitation_handler::respond_to_mcp_elicitation(
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
                let events = drain_mcp_connection_callback_observations().await;
                let request_id = match events.as_slice() {
                    [crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ElicitationRequested { event }] => event.request_id.clone(),
                    other => panic!("expected one URL elicitation event, got {other:?}"),
                };
                assert!(
                    crate::services::mcp::elicitation_handler::respond_to_mcp_elicitation(
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
                        "URL elicitation was declined by the user. The tool \"login\" could not complete because it requires the user to open a URL."
                    )
                );

                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

    #[test]
    fn stdio_url_elicitation_required_retries_after_user_accepts() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // Elicitation notification hooks are detached onto it.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let script_path =
            std::env::temp_dir().join(format!("nyan-mcp-url-elicit-{}.mjs", uuid::Uuid::new_v4()));
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
          message: 'URL elicitation required: Unauthorized',
          data: {
            elicitations: [{
              mode: 'url',
              url: 'https://example.com/auth?state=401',
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
                let _ = drain_mcp_connection_callback_observations().await;
                runtime::clear_mcp_auth_cache();
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(tool_names(&discovery.tools), vec!["login"]);

                let client = discovery
                    .server
                    .as_connected()
                    .expect("a connected record")
                    .client
                    .clone();
                let call = tokio::spawn(async move {
                    runtime::call_mcp_tool_with_elicitation(
                        &client,
                        "login",
                        Map::new(),
                        None,
                        crate::tool::HandleElicitationCallback::default(),
                    )
                    .await
                });
                let request_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let event = drain_mcp_connection_callback_observations()
                            .await
                            .into_iter()
                            .find_map(|observation| match observation {
                                crate::services::mcp::use_manage_mcp_connections::McpConnectionCallbackObservation::ElicitationRequested { event } => {
                                    Some(event)
                                }
                            });
                        if let Some(event) = event {
                            assert_eq!(event.server_name, SERVER);
                            assert_eq!(event.request_id, "error-elicit-fixture-elicit");
                            assert!(event.waiting_state.is_some());
                            return event.request_id;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("URL elicitation event should be queued");

                assert!(
                    crate::services::mcp::elicitation_handler::respond_to_mcp_elicitation(
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
                // CC judges auth by a 401 status or `UnauthorizedError`
                // (`client.ts:3197-3199`); the elicitation's "401" and
                // "Unauthorized" text is no auth failure.
                assert!(!runtime::is_mcp_auth_cached(SERVER));
                runtime::clear_server_cache(SERVER, None).await;
                let _ = drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    /// A turn-scoped runtime driving the test, with the process runtime the
    /// memoized connections and fetches run on published.
    fn block_on_with_process_runtime(test: impl std::future::Future<Output = ()>) {
        crate::utils::process_runtime::initialize_test_process_runtime();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(test);
    }

    /// The server's own names of fetched MCP tools (`mcpInfo.toolName`).
    fn tool_names(tools: &[crate::types::tools::Tool]) -> Vec<&str> {
        tools
            .iter()
            .map(|tool| {
                tool.mcp_info
                    .as_ref()
                    .map_or(tool.name.as_str(), |info| info.tool_name.as_str())
            })
            .collect()
    }

    /// CC `connectToServer` is memoized by `getServerCacheKey(name,
    /// serverRef)`: concurrent and later calls for the same name and config
    /// share one connection, which carries no tools — callers fetch those —
    /// and `clearServerCache` makes the next call connect again.
    #[test]
    fn connect_to_server_is_memoized_by_name_and_config() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "memo-stdio-fixture";
            let config = fixture.config(&[]);
            let (first, second) = tokio::join!(
                runtime::connect_to_server(SERVER, &config),
                runtime::connect_to_server(SERVER, &config)
            );
            assert_eq!(
                first.connection_type(),
                McpServerConnectionType::Connected,
                "{first:?}"
            );
            assert_eq!(connection_id(&first), connection_id(&second));
            let third = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(connection_id(&third), connection_id(&first));
            let first_client = &first.as_connected().expect("connected").client;
            assert!(!first_client.is_closed());
            assert_eq!(fixture.count("initialize"), 1);
            assert_eq!(fixture.count("tools/list"), 0);

            runtime::clear_server_cache(SERVER, Some(&config)).await;
            assert!(first_client.is_closed());
            assert!(memoized_connection_ids(SERVER).is_empty());
            let fresh = runtime::connect_to_server(SERVER, &config).await;
            assert_ne!(connection_id(&fresh), connection_id(&first));
            assert_eq!(fixture.count("initialize"), 2);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC `client.ts:1159-1171`: `rawInstructions.length` counts UTF-16 code
    /// units, and longer instructions are cut at 2048 of them and marked.
    #[test]
    fn server_instructions_are_cut_at_2048_utf16_code_units() {
        let short = "x".repeat(MAX_MCP_DESCRIPTION_LENGTH);
        assert_eq!(runtime::truncate_mcp_instructions(&short), short);

        let long = "x".repeat(3000);
        assert_eq!(
            runtime::truncate_mcp_instructions(&long),
            format!("{}… [truncated]", "x".repeat(MAX_MCP_DESCRIPTION_LENGTH))
        );

        // 1500 characters but 3000 code units: over the limit as CC counts.
        let astral = "😀".repeat(1500);
        assert_eq!(
            runtime::truncate_mcp_instructions(&astral),
            format!("{}… [truncated]", "😀".repeat(1024))
        );
    }

    /// CC's `Client.connect` sends `initialize` at the SDK's
    /// `LATEST_PROTOCOL_VERSION`, 2025-11-25. rmcp's own default is the
    /// 2026-07-28 draft, which has no `initialize` handshake.
    #[test]
    fn initialize_requests_the_sdk_latest_protocol_version() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "protocol-version-fixture";
            let config = fixture.config(&[]);
            let connected = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(
                connected.connection_type(),
                McpServerConnectionType::Connected,
                "{connected:?}"
            );
            assert_eq!(fixture.count("protocolVersion 2025-11-25"), 1);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC's SDK keeps no response cache: the second `resources/read` goes to
    /// the server although the first answer carried a one-minute `ttlMs`, and
    /// its error reaches the caller instead of the earlier contents.
    #[test]
    fn resource_reads_go_to_the_server_every_time() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "resource-cache-fixture";
            let config = fixture.config(&["resources"]);
            let connected = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(
                connected.connection_type(),
                McpServerConnectionType::Connected,
                "{connected:?}"
            );

            let client = &connected.as_connected().expect("connected").client;
            let first = runtime::read_mcp_resource(client, "memo://doc")
                .await
                .expect("first read");
            assert_eq!(first["contents"][0]["text"], "fresh");
            let second = runtime::read_mcp_resource(client, "memo://doc").await;
            assert!(
                second
                    .as_ref()
                    .is_err_and(|error| error.to_string().contains("read failed")),
                "{second:?}"
            );
            assert_eq!(fixture.count("resources/read"), 2);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// `notifications/elicitation/complete` reaches the completion handler
    /// `onConnectionAttempt` registers (`elicitationHandler.ts:175-207`),
    /// although rmcp no longer types it: the queued request it names is
    /// marked complete.
    #[test]
    fn elicitation_complete_notification_reaches_the_completion_handler() {
        use crate::services::mcp::elicitation_handler::{
            ElicitationRequestEvent, ElicitationRequestParams,
        };
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "elicitation-complete-fixture";
            let config = fixture.config(&[]);
            let connected = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(
                connected.connection_type(),
                McpServerConnectionType::Connected,
                "{connected:?}"
            );
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let writer = crate::state::app_state_store::McpWriter::new(store.clone());
            writer.push_elicitation_event(ElicitationRequestEvent::new(
                SERVER,
                "1",
                ElicitationRequestParams::Url {
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: Some("elicit-1".to_string()),
                },
            ));
            crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                McpConnectionDiscovery::from(connected.clone()),
                &writer,
            );

            // The fixture answers a `complete` call with the notification.
            let client = &connected.as_connected().expect("a connected record").client;
            runtime::call_mcp_tool(client, "complete", Map::new(), None)
                .await
                .expect("tool call");
            until_state(&store, "the queued elicitation to complete", |state| {
                state
                    .elicitation
                    .queue
                    .first()
                    .is_some_and(|event| event.completed)
            })
            .await;

            runtime::clear_server_cache(SERVER, None).await;
        });
    }

    /// An `elicitation/create` from the server: before `onConnectionAttempt`
    /// the default handler `connectToServer` registered cancels it
    /// (`client.ts:1188-1197`); after, `registerElicitationHandler`'s handler
    /// queues it for the dialog and answers with the dialog's result
    /// (`elicitationHandler.ts:77-171`), and a request the server cancels
    /// leaves the queue. A URL-mode request is refused either way: the
    /// client declares form mode only (SDK `client/index.js:191-207`).
    #[test]
    fn server_elicitations_are_cancelled_until_the_session_registers_its_handler() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path =
            std::env::temp_dir().join(format!("nyan-mcp-elicit-{}.mjs", uuid::Uuid::new_v4()));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
function reply(id, text) {
  send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text }] } })
}
let pendingCall
rl.on('line', line => {
  let message
  try { message = JSON.parse(line) } catch { return }
  if (message.method === undefined) {
    // The client's answer to an `elicitation/create`.
    if (message.id === 'srv-1') {
      reply(pendingCall, message.error ? `error:${message.error.code}` : message.result.action)
    }
    return
  }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: { tools: {} },
        serverInfo: { name: 'elicit-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/call') {
    const name = message.params.name
    if (name === 'ask' || name === 'ask-url') {
      pendingCall = message.id
      send({
        jsonrpc: '2.0',
        id: 'srv-1',
        method: 'elicitation/create',
        params: name === 'ask'
          ? {
              message: 'Pick one',
              requestedSchema: { type: 'object', properties: { choice: { type: 'string' } } }
            }
          : { mode: 'url', message: 'Open', url: 'https://example.com/auth', elicitationId: 'url-1' }
      })
    } else if (name === 'hold') {
      send({
        jsonrpc: '2.0',
        id: 'srv-2',
        method: 'elicitation/create',
        params: { message: 'Hold', requestedSchema: { type: 'object', properties: {} } }
      })
      reply(message.id, 'held')
    } else if (name === 'cancel') {
      send({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 'srv-2' } })
      reply(message.id, 'cancelled')
    }
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#,
        )
        .expect("write MCP fixture");

        block_on_with_process_runtime(async {
            const SERVER: &str = "elicit-stdio-fixture";
            let mut config = ScopedMcpServerConfig::stdio_for_test("node");
            config.args = vec![script_path.to_string_lossy().to_string()];
            let connected = runtime::connect_to_server(SERVER, &config).await;
            let client = connected
                .as_connected()
                .expect("a connected record")
                .client
                .clone();
            // What the fixture answers a tool call with: the action of the
            // client's answer, or `error:<code>`.
            async fn call(client: &McpClient, tool: &str) -> Option<String> {
                let value = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    runtime::call_mcp_tool(client, tool, Map::new(), None),
                )
                .await
                .expect("the tool call returns")
                .expect("tool call");
                value["content"][0]["text"].as_str().map(str::to_owned)
            }

            assert_eq!(call(&client, "ask").await.as_deref(), Some("cancel"));
            assert_eq!(
                call(&client, "ask-url").await.as_deref(),
                Some("error:-32602")
            );

            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let writer = crate::state::app_state_store::McpWriter::new(store.clone());
            crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                McpConnectionDiscovery::from(connected.clone()),
                &writer,
            );
            assert_eq!(
                call(&client, "ask-url").await.as_deref(),
                Some("error:-32602")
            );
            assert!(store.get().elicitation.queue.is_empty());

            let asking = tokio::spawn({
                let client = client.clone();
                async move { call(&client, "ask").await }
            });
            until_state(&store, "the queued elicitation", |state| {
                !state.elicitation.queue.is_empty()
            })
            .await;
            let event = store.get().elicitation.queue[0].clone();
            assert_eq!(event.server_name, SERVER);
            assert_eq!(event.request_id, "srv-1");
            assert!(matches!(
                &event.params,
                crate::services::mcp::elicitation_handler::ElicitationRequestParams::Form {
                    message,
                    ..
                } if message == "Pick one"
            ));
            assert_eq!(event.waiting_state, None);
            assert!(
                crate::services::mcp::elicitation_handler::respond_to_mcp_elicitation(
                    SERVER,
                    &event.request_id,
                    ElicitationResult::new(ElicitationAction::Accept),
                )
                .await
            );
            assert_eq!(asking.await.unwrap().as_deref(), Some("accept"));

            // The server cancels a request still in the dialog's queue.
            let held = |state: &crate::state::app_state_store::AppState| {
                state
                    .elicitation
                    .queue
                    .iter()
                    .any(|event| event.request_id == "srv-2")
            };
            assert_eq!(call(&client, "hold").await.as_deref(), Some("held"));
            until_state(&store, "the held elicitation", held).await;
            assert_eq!(call(&client, "cancel").await.as_deref(), Some("cancelled"));
            until_state(&store, "the cancelled elicitation to leave", |state| {
                !held(state)
            })
            .await;
            assert!(
                !crate::services::mcp::elicitation_handler::respond_to_mcp_elicitation(
                    SERVER,
                    "srv-2",
                    ElicitationResult::new(ElicitationAction::Accept),
                )
                .await
            );

            runtime::clear_server_cache(SERVER, None).await;
        });
        let _ = std::fs::remove_file(script_path);
    }

    /// Without a process runtime a memoized connection would live on the
    /// caller's runtime, which may end with the turn, so each call connects
    /// for itself.
    #[test]
    fn connect_to_server_without_a_process_runtime_is_not_memoized() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                const SERVER: &str = "unmemoized-stdio-fixture";
                let config = fixture.config(&[]);
                let first = runtime::connect_to_server(SERVER, &config).await;
                let second = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(second.connection_type(), McpServerConnectionType::Connected);
                assert_ne!(connection_id(&first), connection_id(&second));
                assert_eq!(fixture.count("initialize"), 2);
                // Each is its caller's alone: a clear does not reach it.
                runtime::clear_server_cache(SERVER, None).await;
                for record in [&first, &second] {
                    let client = &record.as_connected().expect("connected").client;
                    assert!(!client.is_closed());
                    runtime::cleanup_connection(client);
                    assert!(client.is_closed());
                }
                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

    /// CC's `onclose` wrapper (`client.ts:1374-1402`): a connection that
    /// ends leaves the memo and takes the name's fetch caches with it, so the
    /// next call connects and fetches again.
    #[test]
    fn a_closed_connection_leaves_the_memo_and_the_fetch_caches() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "closing-stdio-fixture";
            let config = fixture.config(&[]);
            let first = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&first).await),
                ["lookup_1"]
            );

            // Any tool call makes the fixture exit.
            let client = &first.as_connected().expect("a connected record").client;
            let _ = runtime::call_mcp_tool(client, "lookup_1", Map::new(), None).await;
            fixture
                .until("the closed connection to leave the memo", |_| {
                    memoized_connection_ids(SERVER).is_empty()
                })
                .await;
            assert!(client.is_closed());

            let second = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(second.connection_type(), McpServerConnectionType::Connected);
            assert_ne!(connection_id(&second), connection_id(&first));
            assert_eq!(fixture.count("initialize"), 2);
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&second).await),
                ["lookup_1"]
            );
            assert_eq!(fixture.count("tools/list"), 2);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC's `onclose` (`useManageMCPConnections.ts:333-468`) learns of a
    /// connection that ends. Here it runs only for one that ends on its own:
    /// one closed on purpose — `cleanup()` or `clearServerCache` — does not
    /// run it, and neither does one whose `onclose` was cleared (`:809`).
    /// One that ends on its own takes the name's fetch caches with it
    /// (`client.ts:1389-1391`), so another connection under the name fetches
    /// again.
    #[test]
    fn only_a_connection_that_ends_on_its_own_runs_its_onclose() {
        use futures::FutureExt as _;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "onclose-stdio-fixture";
            let (closed, closes) = async_channel::unbounded::<&'static str>();
            // CC `client.client.onclose = () => { ... }`, here recording
            // which connection's ran.
            let assign_onclose = |client: &McpClient, label: &'static str| {
                let closed = closed.clone();
                client.set_onclose(move || {
                    let closed = closed.clone();
                    async move {
                        let _ = closed.send(label).await;
                    }
                    .boxed()
                });
            };

            // Any tool call makes the fixture exit.
            let cleaned = runtime::connect_to_server(SERVER, &fixture.config(&["cleaned"])).await;
            let cleaned = &cleaned.as_connected().expect("connected").client;
            assign_onclose(cleaned, "cleaned");
            runtime::cleanup_connection(cleaned);
            assert!(cleaned.is_closed());

            let cleared_config = fixture.config(&["cleared"]);
            let cleared = runtime::connect_to_server(SERVER, &cleared_config).await;
            let cleared = &cleared.as_connected().expect("connected").client;
            assign_onclose(cleared, "cleared");
            runtime::clear_server_cache(SERVER, Some(&cleared_config)).await;
            assert!(cleared.is_closed());

            let silenced_config = fixture.config(&["silenced"]);
            let silenced = runtime::connect_to_server(SERVER, &silenced_config).await;
            let silenced = &silenced.as_connected().expect("connected").client;
            assign_onclose(silenced, "silenced");
            silenced.clear_onclose();
            let _ = runtime::call_mcp_tool(silenced, "lookup", Map::new(), None).await;
            fixture
                .until("the silenced connection to leave the memo", |_| {
                    !memoized_connection_ids(SERVER).contains(&silenced.connection_id())
                })
                .await;

            // Another connection under the name, its tools fetched once and
            // then cached.
            let bystander =
                runtime::connect_to_server(SERVER, &fixture.config(&["bystander"])).await;
            for _ in 0..2 {
                assert_eq!(
                    tool_names(&runtime::fetch_tools_for_client(&bystander).await),
                    ["lookup_1"]
                );
            }
            let lists = fixture.count("tools/list");

            let exiting = runtime::connect_to_server(SERVER, &fixture.config(&["exiting"])).await;
            let exiting = &exiting.as_connected().expect("connected").client;
            assign_onclose(exiting, "exiting");
            let _ = runtime::call_mcp_tool(exiting, "lookup", Map::new(), None).await;
            // The first `onclose` to run is the exited connection's. Any
            // other would have run when its own connection ended, which was
            // well before; `try_recv` then only sees nothing has run since.
            let first = tokio::time::timeout(std::time::Duration::from_secs(5), closes.recv())
                .await
                .expect("the exited connection runs its onclose")
                .unwrap();
            assert_eq!(first, "exiting");
            assert!(closes.try_recv().is_err());
            assert!(exiting.is_closed());

            // The exited connection took the name's fetch caches.
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&bystander).await),
                ["lookup_2"]
            );
            assert_eq!(fixture.count("tools/list"), lists + 1);

            runtime::clear_server_cache(SERVER, None).await;
        });
    }

    /// CC `onConnectionAttempt` assigns a connected client's `onclose`
    /// (`useManageMCPConnections.ts:333-468`): when a stdio server the
    /// session connected exits, its record is marked failed (`:466`). A
    /// connection of the same name made outside it, as an agent's inline
    /// server is (`runAgent.ts:173`), has no `onclose`.
    #[test]
    fn the_session_marks_its_server_failed_through_the_onclose_it_assigned() {
        use crate::services::mcp::types::ConfigScope;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "session-onclose-stdio-fixture";
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let writer = crate::state::app_state_store::McpWriter::new(store.clone());
            let record_type = || {
                store
                    .get()
                    .mcp
                    .clients
                    .iter()
                    .find(|server| server.name() == SERVER)
                    .map(McpServerConnection::connection_type)
            };

            let session = runtime::connect_to_server(SERVER, &fixture.config(&["session"])).await;
            crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                McpConnectionDiscovery::from(session.clone()),
                &writer,
            );
            let session = &session.as_connected().expect("connected").client;
            assert!(session.onclose().is_some());

            let inline_config = ScopedMcpServerConfig {
                scope: ConfigScope::Dynamic,
                ..fixture.config(&["inline"])
            };
            let inline = runtime::connect_to_server(SERVER, &inline_config).await;
            let inline = &inline.as_connected().expect("connected").client;
            assert_ne!(inline, session);
            assert!(inline.onclose().is_none());

            // Any tool call makes the fixture exit.
            let _ = runtime::call_mcp_tool(session, "lookup", Map::new(), None).await;
            fixture
                .until("the session's record to be marked failed", |_| {
                    record_type() == Some(McpServerConnectionType::Failed)
                })
                .await;

            runtime::clear_server_cache(SERVER, None).await;
        });
    }

    /// CC's `clearServerCache` is a promise: it closes the connections it
    /// took whether or not anyone awaits it. A dropped clear still closes
    /// them, so none is left open outside the memo.
    #[test]
    fn a_dropped_clear_still_closes_what_it_took() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "dropped-clear-stdio-fixture";
            let config = fixture.config(&[]);
            let record = runtime::connect_to_server(SERVER, &config).await;
            let client = &record.as_connected().expect("connected").client;

            drop(runtime::clear_server_cache(SERVER, Some(&config)));
            assert!(memoized_connection_ids(SERVER).is_empty());
            fixture
                .until("the dropped clear to close the connection", |_| {
                    client.is_closed()
                })
                .await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// A connection still being made when `clearServerCache` takes its memo
    /// entry is closed once made, as CC's clear awaits the connect and then
    /// `cleanup()`s it — here even when the clear's future is dropped: a JS
    /// promise runs on unawaited. Its waiters get it as connected; fetches
    /// through it come back empty. A connection made after the clear stays
    /// open and memoized. The fixture answers the first `initialize` late.
    #[test]
    fn a_connection_cleared_while_connecting_is_closed_once_made() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "slow-stdio-fixture";
            let config = fixture.config(&["slow-first"]);
            let first = tokio::spawn({
                let config = config.clone();
                async move { runtime::connect_to_server(SERVER, &config).await }
            });
            fixture
                .until("the first initialize", |fixture| {
                    fixture.count("initialize") == 1
                })
                .await;

            drop(runtime::clear_server_cache(SERVER, Some(&config)));
            let second = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(second.connection_type(), McpServerConnectionType::Connected);
            let first = first.await.unwrap();

            assert_eq!(first.connection_type(), McpServerConnectionType::Connected);
            assert_ne!(connection_id(&first), connection_id(&second));
            assert!(first.as_connected().expect("connected").client.is_closed());
            assert!(!second.as_connected().expect("connected").client.is_closed());
            assert!(runtime::fetch_tools_for_client(&first).await.is_empty());
            assert_eq!(fixture.count("initialize"), 2);
            assert_eq!(
                memoized_connection_ids(SERVER),
                Vec::from_iter(connection_id(&second))
            );
            assert_eq!(
                connection_id(&runtime::connect_to_server(SERVER, &config).await),
                connection_id(&second)
            );

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// lodash `memoize` caches the promise whatever it settles to, so CC's
    /// failed connection is the answer until `clearServerCache` —
    /// `reconnectMcpServerImpl` — deletes it.
    #[test]
    fn a_failed_connection_stays_memoized_until_cleared() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "failing-stdio-fixture";
            let config = fixture.config(&["exit-on-initialize"]);
            let first = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(first.connection_type(), McpServerConnectionType::Failed);
            let again = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(again.connection_type(), McpServerConnectionType::Failed);
            assert_eq!(fixture.count("initialize"), 1);

            let reconnected = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
            assert_eq!(
                reconnected.server.connection_type(),
                McpServerConnectionType::Failed
            );
            assert_eq!(fixture.count("initialize"), 2);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC `ensureConnectedClient` (`client.ts:1688-1704`): the memoized
    /// connection while it is healthy, a fresh one after a clear, and "not
    /// connected" when the memoized result is not a connection.
    #[test]
    fn ensure_connected_client_reuses_reconnects_or_reports_not_connected() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "ensure-stdio-fixture";
            let config = fixture.config(&[]);
            let client = runtime::connect_to_server(SERVER, &config).await;
            let client = client.as_connected().expect("a healthy connection");
            let same = runtime::ensure_connected_client(client)
                .await
                .expect("a healthy connection");
            assert_eq!(same.client.connection_id(), client.client.connection_id());
            assert_eq!(fixture.count("initialize"), 1);

            runtime::clear_server_cache(SERVER, Some(&config)).await;
            let fresh = runtime::ensure_connected_client(client)
                .await
                .expect("a fresh connection");
            assert_ne!(fresh.client.connection_id(), client.client.connection_id());
            assert_eq!(fixture.count("initialize"), 2);
            runtime::clear_server_cache(SERVER, None).await;

            const FAILING: &str = "ensure-failing-stdio-fixture";
            let mut failing = client.clone();
            failing.name = FAILING.to_string();
            failing.config = fixture.config(&["exit-on-initialize"]);
            let error = runtime::ensure_connected_client(&failing)
                .await
                .expect_err("the server exits during initialize");
            assert_eq!(
                error.to_string(),
                format!("MCP server \"{FAILING}\" is not connected")
            );
            runtime::clear_server_cache(FAILING, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC's `onerror` closes the transport of an expired session whatever the
    /// request was (`client.ts:1313-1327`): the prompt that hit it fails, and
    /// the next one reconnects through `ensureConnectedClient`.
    #[test]
    fn an_expired_session_on_a_prompt_closes_its_connection() {
        use session_http_fixture::{SESSION_NOT_FOUND, SessionHttpFixture};
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        block_on_with_process_runtime(async {
            let fixture = SessionHttpFixture::spawn(
                |method, session| method == "prompts/get" && session == "s1",
                SESSION_NOT_FOUND,
            )
            .await;
            const SERVER: &str = "prompt-session-http-fixture";
            let config = fixture.config();
            let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
            let client = discovery.server.as_connected().expect("connected");

            runtime::get_mcp_prompt_for_command(client, "daily", &[], "")
                .await
                .expect_err("the session expired");
            let blocks = runtime::get_mcp_prompt_for_command(client, "daily", &[], "")
                .await
                .expect("a fresh session");
            assert_eq!(blocks[0]["text"].as_str(), Some("prompt:s2"));
            assert_eq!(
                fixture.log(),
                [
                    "initialize s1",
                    "prompts/get s1",
                    "initialize s2",
                    "prompts/get s2"
                ]
            );

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC's MCP tool closes over the `client` it was fetched from and calls
    /// over `ensureConnectedClient(client)` (`client.ts:1858-1862`). After a
    /// reconnect, the `tools/list_changed` refetch's tools carry the new
    /// connection and their calls go out on it. A tool still bound to the
    /// closed connection reaches whatever connection is memoized for its
    /// name and config now, not the closed one.
    #[test]
    fn tools_refetched_after_a_reconnect_call_the_new_connection() {
        use session_http_fixture::{SESSION_NOT_FOUND, SessionHttpFixture};
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        block_on_with_process_runtime(async {
            // No request expires: each connection keeps its session.
            let fixture = SessionHttpFixture::spawn(|_, _| false, SESSION_NOT_FOUND).await;
            const SERVER: &str = "list-changed-session-http-fixture";
            let config = fixture.config();

            // Connection A (session s1) and the tools fetched from it.
            let first_record = runtime::connect_to_server(SERVER, &config).await;
            let first = first_record.as_connected().expect("connection A");
            let first_tools = runtime::fetch_tools_for_client(&first_record).await;
            assert_eq!(tool_names(&first_tools), ["lookup"]);
            let first_binding = bound_client(&first_tools[0]);
            assert_eq!(first_binding.client, first.client);

            // A reconnect (`reconnectMcpServerImpl`'s clear, then connect)
            // makes connection B (session s2); B's `tools/list_changed`
            // handler then refetches the tools on B.
            runtime::clear_server_cache(SERVER, Some(&config)).await;
            let second_record = runtime::connect_to_server(SERVER, &config).await;
            let second = second_record.as_connected().expect("connection B");
            assert_ne!(second.client, first.client);
            let second_tools = runtime::refresh_mcp_tools_for_client(&second_record).await;
            assert_eq!(tool_names(&second_tools), ["lookup"]);
            let second_binding = bound_client(&second_tools[0]);
            // Two fetches, two captures: the bindings are different objects,
            // and the new one is the record `connectToServer` returned.
            assert!(!std::sync::Arc::ptr_eq(&first_binding, &second_binding));
            assert_eq!(&*second_binding, second);

            // The tool's `call`: `ensureConnectedClient(client)`, then the
            // request on the connection it returns.
            async fn call_lookup(binding: &ConnectedMcpServer) -> (McpClient, Value) {
                let connected = runtime::ensure_connected_client(binding)
                    .await
                    .expect("a connection");
                let value = runtime::call_mcp_tool_with_elicitation(
                    &connected.client,
                    "lookup",
                    Map::new(),
                    None,
                    crate::tool::HandleElicitationCallback::default(),
                )
                .await
                .expect("the call succeeds");
                (connected.client, value)
            }

            let (client, value) = call_lookup(&second_binding).await;
            assert_eq!(client, second.client);
            assert_eq!(value["content"][0]["text"].as_str(), Some("ok:s2"));

            // A tool still bound to A gets the connection memoized for its
            // name and config now, B, not the closed A.
            let (client, value) = call_lookup(&first_binding).await;
            assert_eq!(client, second.client);
            assert_eq!(value["content"][0]["text"].as_str(), Some("ok:s2"));

            // A request on A itself, which is closed, fails at the transport;
            // on http that is an expired session (CC `client.ts:3210-3231`
            // treats -32000 "Connection closed" so). Only A is closed: B,
            // under the same name and config, stays open and memoized, where
            // CC's `clearServerCache(name, config)` would take it too.
            let error = runtime::call_mcp_tool_with_elicitation(
                &first.client,
                "lookup",
                Map::new(),
                None,
                crate::tool::HandleElicitationCallback::default(),
            )
            .await
            .expect_err("A is closed");
            assert!(error.is::<McpSessionExpiredError>(), "{error:#}");
            assert!(!second.client.is_closed());
            assert_eq!(
                memoized_connection_ids(SERVER),
                [second.client.connection_id()]
            );

            assert_eq!(
                fixture.log(),
                [
                    "initialize s1",
                    "initialize s2",
                    "tools/call s2",
                    "tools/call s2"
                ]
            );

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC's memo is keyed by name and config (`client.ts:581-595`): another
    /// config's connection under a taken name is a second connection beside
    /// the first, and each fetches its own tools. Closing one leaves the
    /// other open and memoized.
    #[test]
    fn connections_under_one_name_with_different_configs_coexist() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "coexisting-stdio-fixture";
            let first_config = fixture.config(&[]);
            let second_config = fixture.config(&["another-config"]);
            let first = runtime::connect_to_server(SERVER, &first_config).await;
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&first).await),
                ["lookup_1"]
            );

            let second = runtime::connect_to_server(SERVER, &second_config).await;
            assert_eq!(second.connection_type(), McpServerConnectionType::Connected);
            assert_ne!(connection_id(&second), connection_id(&first));
            let first_client = &first.as_connected().expect("connected").client;
            let second_client = &second.as_connected().expect("connected").client;
            assert!(!first_client.is_closed());
            assert!(!second_client.is_closed());
            assert_eq!(
                memoized_connection_ids(SERVER),
                [first_client.connection_id(), second_client.connection_id()]
            );
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&second).await),
                ["lookup_1"]
            );
            assert_eq!(fixture.count("tools/list"), 2);

            runtime::cleanup_connection(second_client);
            assert!(second_client.is_closed());
            assert!(!first_client.is_closed());
            assert_eq!(
                memoized_connection_ids(SERVER),
                [first_client.connection_id()]
            );
            assert_eq!(
                connection_id(&runtime::connect_to_server(SERVER, &first_config).await),
                connection_id(&first)
            );
            assert_eq!(fixture.count("initialize"), 2);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// CC `fetchToolsForClient` is `memoizeWithLRU`'d by `client.name`:
    /// concurrent and later fetches share one `tools/list`, the
    /// `tools/list_changed` refresh deletes the entry and fetches again, and
    /// `clearServerCache` deletes it with the connection.
    #[test]
    fn tool_fetches_are_memoized_by_name_until_refreshed_or_cleared() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = counting_fixture::CountingFixture::new();
        block_on_with_process_runtime(async {
            const SERVER: &str = "fetch-stdio-fixture";
            let config = fixture.config(&[]);
            let client = runtime::connect_to_server(SERVER, &config).await;
            let (first, second) = tokio::join!(
                runtime::fetch_tools_for_client(&client),
                runtime::fetch_tools_for_client(&client)
            );
            assert_eq!(tool_names(&first), ["lookup_1"]);
            assert_eq!(first, second);
            assert_eq!(fixture.count("tools/list"), 1);

            let refreshed = runtime::refresh_mcp_tools_for_client(&client).await;
            assert_eq!(tool_names(&refreshed), ["lookup_2"]);
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&client).await),
                ["lookup_2"]
            );
            assert_eq!(fixture.count("tools/list"), 2);

            runtime::clear_server_cache(SERVER, Some(&config)).await;
            let fresh = runtime::connect_to_server(SERVER, &config).await;
            assert_eq!(
                tool_names(&runtime::fetch_tools_for_client(&fresh).await),
                ["lookup_1"]
            );
            assert_eq!(fixture.count("tools/list"), 3);
            // The old record's connection is closed.
            assert!(runtime::fetch_tools_for_client(&client).await.is_empty());
            assert_eq!(fixture.count("tools/list"), 3);

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
    }

    /// Waits until `condition` holds of the store's state; fails after five
    /// seconds.
    async fn until_state(
        store: &crate::state::store::AppStore,
        what: &str,
        condition: impl Fn(&crate::state::app_state_store::AppState) -> bool,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !condition(&store.get()) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    /// CC registers a list_changed handler only for a list the server
    /// declared can change (`useManageMCPConnections.ts:618`, `:667`,
    /// `:705`), and only through `onConnectionAttempt`.
    #[test]
    fn list_changed_handlers_follow_the_declared_capabilities() {
        use rmcp::model::{
            ConstString as _, PromptListChangedNotificationMethod,
            ResourceListChangedNotificationMethod, ToolListChangedNotificationMethod,
        };
        let mut server = McpServerConnection::connected_for_test(
            "docs",
            ScopedMcpServerConfig::stdio_for_test("docs"),
        );
        if let McpServerConnection::Connected(connected) = &mut server {
            let mut tools = rmcp::model::ToolsCapability::default();
            tools.list_changed = Some(true);
            connected.capabilities.tools = Some(tools);
            connected.capabilities.prompts = Some(Default::default());
        }
        let client = server.as_connected().expect("connected").client.clone();
        assert!(
            client
                .notification_handler(ToolListChangedNotificationMethod::VALUE)
                .is_none()
        );

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let writer = crate::state::app_state_store::McpWriter::new(store);
        crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
            McpConnectionDiscovery::from(server),
            &writer,
        );
        assert!(
            client
                .notification_handler(ToolListChangedNotificationMethod::VALUE)
                .is_some()
        );
        assert!(
            client
                .notification_handler(PromptListChangedNotificationMethod::VALUE)
                .is_none()
        );
        assert!(
            client
                .notification_handler(ResourceListChangedNotificationMethod::VALUE)
                .is_none()
        );
    }

    /// A list_changed handler lives on the connection it refreshes and holds
    /// it weakly: once the state moves on from its record, nothing keeps the
    /// connection's handle alive.
    #[test]
    fn list_changed_handlers_do_not_keep_their_connection_alive() {
        let mut server = McpServerConnection::connected_for_test(
            "docs",
            ScopedMcpServerConfig::stdio_for_test("docs"),
        );
        if let McpServerConnection::Connected(connected) = &mut server {
            let mut tools = rmcp::model::ToolsCapability::default();
            tools.list_changed = Some(true);
            connected.capabilities.tools = Some(tools);
        }
        let weak = server.as_connected().expect("connected").client.downgrade();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let writer = crate::state::app_state_store::McpWriter::new(store);
        crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
            McpConnectionDiscovery::from(server),
            &writer,
        );
        {
            use rmcp::model::ConstString as _;
            let record = weak.connected_record().expect("the record is in the state");
            assert!(
                record
                    .as_connected()
                    .expect("connected")
                    .client
                    .notification_handler(rmcp::model::ToolListChangedNotificationMethod::VALUE)
                    .is_some()
            );
        }

        let config = ScopedMcpServerConfig::stdio_for_test("docs");
        writer.apply_client_update(McpConnectionDiscovery::disabled("docs", &config).server);
        assert!(weak.connected_record().is_none());
    }

    /// CC's `onConnectionAttempt` registers a `tools/list_changed` handler on
    /// a client whose server declares `tools.listChanged`
    /// (`useManageMCPConnections.ts:618-665`): the notification refetches
    /// the tools on that client and writes them with its record. A
    /// connection made outside `onConnectionAttempt` has no handler.
    #[test]
    fn stdio_tools_list_changed_notification_refreshes_live_tools() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "nyan-mcp-tools-list-changed-{}.mjs",
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
  } else if (message.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: 'ok' }] } })
    send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' })
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
                    plugin_source: None,
                };
                let store = crate::state::store::AppStore::new(
                    crate::state::app_state_store::AppState::default(),
                    None,
                );
                let writer = crate::state::app_state_store::McpWriter::new(store.clone());
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(tool_names(&discovery.tools), vec!["first"]);
                let client = discovery
                    .server
                    .as_connected()
                    .expect("connected")
                    .client
                    .clone();
                {
                    use rmcp::model::ConstString as _;
                    assert!(
                        client
                            .notification_handler(
                                rmcp::model::ToolListChangedNotificationMethod::VALUE
                            )
                            .is_none()
                    );
                }
                crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                    discovery, &writer,
                );
                assert_eq!(tool_names(&store.get().mcp.tools), vec!["first"]);

                // Any tool call makes the fixture announce a changed list.
                runtime::call_mcp_tool(&client, "first", Map::new(), None)
                    .await
                    .expect("the tool call");
                until_state(&store, "the refetched tools", |state| {
                    tool_names(&state.mcp.tools) == ["second"]
                })
                .await;
                // Still this connection's record; which record a handler
                // writes is told apart by the same-name test.
                assert_eq!(
                    store.get().mcp.clients[0]
                        .as_connected()
                        .map(|record| &record.client),
                    Some(&client)
                );
                runtime::clear_server_cache(SERVER, None).await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    /// CC's list_changed handlers close over the `client` they were
    /// registered on (`useManageMCPConnections.ts:616-751`): a list_changed
    /// refetches on that connection and writes `updateServer({ ...client,
    /// .. })`. With another connection open under the same name, each
    /// notification acts for the connection that sent it.
    #[test]
    fn notifications_carry_the_connection_that_sent_them() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path =
            std::env::temp_dir().join(format!("nyan-mcp-notifying-{}.mjs", uuid::Uuid::new_v4()));
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
  if (message.method === 'fixture/emit') {
    send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' })
    return
  }
  if (message.method === 'fixture/emit-resources') {
    send({ jsonrpc: '2.0', method: 'notifications/resources/list_changed' })
    return
  }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: {
          tools: { listChanged: true },
          prompts: {},
          resources: { listChanged: true }
        },
        serverInfo: { name: 'notifying-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { tools: [{ name: 'lookup', inputSchema: { type: 'object', properties: {} } }] }
    })
  } else if (message.method === 'prompts/list') {
    send({ jsonrpc: '2.0', id: message.id, result: { prompts: [{ name: 'daily' }] } })
  } else if (message.method === 'resources/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { resources: [{ uri: 'file:///tmp/notes.txt', name: 'notes' }] }
    })
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#,
        )
        .expect("write MCP fixture");

        block_on_with_process_runtime(async {
            const SERVER: &str = "notifying-stdio-fixture";
            let mut config = ScopedMcpServerConfig::stdio_for_test("node");
            config.args = vec![script_path.to_string_lossy().to_string()];
            let record = runtime::connect_to_server(SERVER, &config).await;
            let sender = record
                .as_connected()
                .expect("a connected record")
                .client
                .clone();

            // Another config's connection under the same name, open beside
            // the sender; any config difference makes a second memo entry.
            let mut other_config = config.clone();
            other_config.plugin_source = Some("other@elsewhere".to_string());
            let other_record = runtime::connect_to_server(SERVER, &other_config).await;
            let other = other_record
                .as_connected()
                .expect("another connection")
                .client
                .clone();
            assert_ne!(other, sender);
            assert!(!sender.is_closed());

            // Both go through the session's `onConnectionAttempt`, the sender
            // last, so the state holds the sender's record.
            let store = crate::state::store::AppStore::new(
                crate::state::app_state_store::AppState::default(),
                None,
            );
            let writer = crate::state::app_state_store::McpWriter::new(store.clone());
            for server in [&other_record, &record] {
                crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                    McpConnectionDiscovery::from(server.clone()),
                    &writer,
                );
            }
            let record_client = |state: &crate::state::app_state_store::AppState| {
                state.mcp.clients[0]
                    .as_connected()
                    .map(|record| record.client.clone())
            };
            assert_eq!(record_client(&store.get()), Some(sender.clone()));

            // Tells `emitter` to notify, then waits until its
            // `tools/list_changed` has written its record.
            async fn notify_through(
                emitter: &McpClient,
                store: &crate::state::store::AppStore,
                record_client: impl Fn(&crate::state::app_state_store::AppState) -> Option<McpClient>,
            ) {
                emitter
                    .notification("fixture/emit", serde_json::json!({}))
                    .await
                    .expect("the fixture is told to notify");
                until_state(store, "the emitter's record", |state| {
                    record_client(state).as_ref() == Some(emitter)
                })
                .await;
            }

            // Each connection under the name in turn: whichever one a lookup
            // by name would find, one of the two turns tells it apart.
            notify_through(&other, &store, record_client).await;
            notify_through(&sender, &store, record_client).await;
            assert_eq!(tool_names(&store.get().mcp.tools), ["lookup"]);

            // The resources handler writes its own record too, and with
            // `MCP_SKILLS` the prompts alongside the resources.
            other
                .notification("fixture/emit-resources", serde_json::json!({}))
                .await
                .expect("the fixture is told to notify");
            until_state(&store, "the other's refetched resources", |state| {
                record_client(state).as_ref() == Some(&other)
                    && state.mcp.resources.contains_key(SERVER)
            })
            .await;
            let state = store.get();
            assert_eq!(state.mcp.clients.len(), 1);
            assert_eq!(
                state
                    .mcp
                    .commands
                    .iter()
                    .map(|command| command.name.to_string())
                    .collect::<Vec<_>>(),
                [mcp_prompt_command_name(SERVER, "daily")]
            );
            assert_eq!(state.mcp.resources[SERVER][0].uri, "file:///tmp/notes.txt");

            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
        let _ = std::fs::remove_file(script_path);
    }

    #[test]
    fn stdio_prompts_and_resources_list_changed_refresh_live_state() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "nyan-mcp-prompts-resources-list-changed-{}.mjs",
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
  } else if (message.method === 'resources/list') {
    resourceListCount += 1
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: { resources: [resource(resourceListCount === 1 ? 'first_resource' : 'second_resource')] }
    })
  } else if (message.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: message.id, result: { content: [] } })
    send({ jsonrpc: '2.0', method: `notifications/${message.params.name}/list_changed` })
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(
                    discovery.commands[0].name,
                    mcp_prompt_command_name(SERVER, "first_prompt")
                );
                assert_eq!(
                    discovery.resources.as_ref().expect("resources")[0].name,
                    "first_resource"
                );

                let client = discovery
                    .server
                    .as_connected()
                    .expect("connected")
                    .client
                    .clone();
                let store = crate::state::store::AppStore::new(
                    crate::state::app_state_store::AppState::default(),
                    None,
                );
                let writer = crate::state::app_state_store::McpWriter::new(store.clone());
                crate::services::mcp::use_manage_mcp_connections::on_connection_attempt(
                    discovery, &writer,
                );

                // The fixture announces a changed list named by the tool
                // called.
                runtime::call_mcp_tool(&client, "prompts", Map::new(), None)
                    .await
                    .expect("the tool call");
                let second_prompt = mcp_prompt_command_name(SERVER, "second_prompt");
                until_state(&store, "the refetched prompts", |state| {
                    state
                        .mcp
                        .commands
                        .iter()
                        .any(|command| command.name == second_prompt)
                })
                .await;
                let commands = store.get().mcp.commands.clone();
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].arg_names, vec!["topic"]);

                runtime::call_mcp_tool(&client, "resources", Map::new(), None)
                    .await
                    .expect("the tool call");
                until_state(&store, "the refetched resources", |state| {
                    state
                        .mcp
                        .resources
                        .get(SERVER)
                        .and_then(|resources| resources.first())
                        .is_some_and(|resource| resource.name == "second_resource")
                })
                .await;
                let state = store.get();
                let resources = &state.mcp.resources[SERVER];
                assert_eq!(resources.len(), 1);
                assert_eq!(resources[0].uri, "file:///tmp/second_resource.txt");
                assert_eq!(resources[0].mime_type.as_deref(), Some("text/plain"));
                // Still this connection's record; which record a handler
                // writes is told apart by the same-name test.
                assert_eq!(
                    state.mcp.clients[0]
                        .as_connected()
                        .map(|record| &record.client),
                    Some(&client)
                );

                runtime::clear_server_cache(SERVER, None).await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[test]
    fn stdio_read_mcp_resource_reads_text_content_from_live_server() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "nyan-mcp-read-resource-{}.mjs",
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
          uri: 'file:///tmp/nyan-resource.txt',
          name: 'nyan-resource',
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
                let _ = drain_mcp_connection_callback_observations().await;
                const SERVER: &str = "read-resource-stdio-fixture";
                let uri = "file:///tmp/nyan-resource.txt";
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                let resources = discovery.resources.as_ref().expect("resources");
                assert_eq!(resources.len(), 1);
                assert_eq!(resources[0].uri, uri);

                let client = &discovery.server.as_connected().expect("connected").client;
                let value = runtime::read_mcp_resource(client, uri)
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
                let _ = drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[test]
    fn stdio_read_mcp_resource_checks_resource_capability_like_official_tool() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "nyan-mcp-read-resource-no-capability-{}.mjs",
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
                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                // `resources.length > 0 ? resources : undefined`.
                assert!(discovery.resources.is_none());

                let client = &discovery.server.as_connected().expect("connected").client;
                let error = runtime::read_mcp_resource(client, "file:///tmp/missing.txt")
                    .await
                    .expect_err("read should be rejected before resources/read");
                assert_eq!(
                    error.to_string(),
                    "Server \"read-resource-no-capability-stdio-fixture\" does not support resources"
                );
                runtime::clear_server_cache(SERVER, None).await;
                let _ = drain_mcp_connection_callback_observations().await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[test]
    fn stdio_get_mcp_prompt_for_command_fetches_live_prompt_content() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path =
            std::env::temp_dir().join(format!("nyan-mcp-get-prompt-{}.mjs", uuid::Uuid::new_v4()));
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

        block_on_with_process_runtime(async {
            let _ = drain_mcp_connection_callback_observations().await;
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
                plugin_source: None,
            };
            let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
            assert_eq!(
                discovery.server.connection_type(),
                McpServerConnectionType::Connected
            );
            assert_eq!(discovery.commands.len(), 1);
            assert_eq!(
                discovery.commands[0].name,
                mcp_prompt_command_name(SERVER, "daily")
            );
            assert_eq!(discovery.commands[0].arg_names, vec!["topic"]);
            // The command carries the client it was fetched from.
            let binding = discovery.commands[0]
                .mcp_prompt
                .as_ref()
                .expect("the prompt carries its client");
            assert_eq!(
                Some(&*binding.connection),
                discovery.server.as_connected(),
                "the fetched record"
            );

            let blocks = runtime::get_mcp_prompt_for_command(
                &binding.connection,
                &binding.prompt_name,
                &["topic".to_string()],
                "roadmap",
            )
            .await
            .expect("prompt get should succeed");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0]["type"].as_str(), Some("text"));
            assert_eq!(blocks[0]["text"].as_str(), Some("summarize:roadmap"));
            runtime::clear_server_cache(SERVER, None).await;
            let _ = drain_mcp_connection_callback_observations().await;
        });
        let _ = std::fs::remove_file(script_path);
    }

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

                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(server_version(&discovery.server), Some("1.0.0"));
                assert_eq!(tool_names(&discovery.tools), vec!["lookup"]);
                assert_eq!(discovery.tools[0].mcp_description, "Lookup docs over HTTP MCP");
                assert_eq!(discovery.server.config().transport, Transport::Http);
                runtime::clear_server_cache(SERVER, None).await;
                let _requests = server.await.expect("fixture server task");
                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

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

                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(server_version(&discovery.server), Some("1.0.0"));
                assert_eq!(tool_names(&discovery.tools), vec!["search"]);
                assert_eq!(discovery.tools[0].mcp_description, "Search docs over SSE MCP");
                assert_eq!(discovery.server.config().transport, Transport::Sse);
                runtime::clear_server_cache(SERVER, None).await;
                let _requests = server.await.expect("fixture server task");
                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

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

                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(server_version(&discovery.server), Some("1.0.0"));
                assert_eq!(tool_names(&discovery.tools), vec!["inspect"]);
                assert_eq!(
                    discovery.tools[0].mcp_description,
                    "Inspect docs over WebSocket MCP"
                );
                assert_eq!(discovery.server.config().transport, Transport::Ws);
                runtime::clear_server_cache(SERVER, None).await;
                server.await.expect("fixture server task");
                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

    #[test]
    fn streamable_http_401_challenge_returns_unavailable_without_credential_or_auth_cache_write() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("nyan-mcp-http-401-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&config_home).expect("create config home");
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &config_home);

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                use tokio::net::TcpListener;

                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let client = runtime::connect_to_server(SERVER, &config).await;
                assert_eq!(client.connection_type(), McpServerConnectionType::Failed);
                assert_eq!(
                    failed_error(&client),
                    Some(
                        crate::constants::oauth::OAUTH_CREDENTIAL_SIDE_EFFECTS_UNAVAILABLE_MESSAGE
                    )
                );
                server.await.expect("fixture server task");
                let _ = drain_mcp_connection_callback_observations().await;
                assert!(!config_home.join(".credentials.json").exists());
                assert!(!config_home.join("mcp-needs-auth-cache.json").exists());
            });

        let _ = std::fs::remove_dir_all(config_home);
    }

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

                let _ = drain_mcp_connection_callback_observations().await;
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
                    plugin_source: None,
                };
                let discovery = runtime::reconnect_mcp_server_impl(SERVER, &config).await;
                assert_eq!(
                    discovery.server.connection_type(),
                    McpServerConnectionType::Connected
                );
                assert_eq!(discovery.tools.len(), 1);
                // The call goes out on the client the tool was fetched from.
                let value = runtime::call_mcp_tool_with_elicitation(
                    &bound_client(&discovery.tools[0]).client,
                    "lookup",
                    Map::from_iter([("query".to_string(), Value::String("manual".to_string()))]),
                    Some(mcp_tool_use_id_meta("toolu_http")),
                    crate::tool::HandleElicitationCallback::default(),
                )
                .await
                .expect("HTTP MCP tool call should succeed");
                assert_eq!(
                    value["content"][0]["text"].as_str(),
                    Some("lookup:manual:toolu_http")
                );
                runtime::clear_server_cache(SERVER, None).await;
                server.await.expect("fixture server task");
                let _ = drain_mcp_connection_callback_observations().await;
            });
    }

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

    #[test]
    fn cached_needs_auth_skips_remote_probe_and_projects_auth_tool() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("nyan-mcp-auth-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &temp_dir);
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
            discoveries[0].server.connection_type(),
            McpServerConnectionType::NeedsAuth
        );
        assert_eq!(tool_names(&discoveries[0].tools), vec!["authenticate"]);
        assert_eq!(discoveries[0].tools[0].name, "mcp__docs__authenticate");
    }

    /// CC's `userFacingName()` reads `tool.annotations?.title` only
    /// (`client.ts:1974`); the tool's top-level `title` is not a fallback.
    #[test]
    fn mcp_tools_take_the_title_from_annotations_only() {
        let top_level_only = mcp_tool_for_test(
            "docs",
            serde_json::json!({
                "name": "lookup",
                "title": "Top Level",
                "inputSchema": {"type": "object"},
                "annotations": {"readOnlyHint": true}
            }),
        );
        assert_eq!(top_level_only.mcp_annotations.title, None);
        assert!(top_level_only.mcp_annotations.read_only_hint);
        assert_eq!(
            top_level_only.mcp_user_facing_name().as_deref(),
            Some("docs - lookup (MCP)")
        );
        let annotated = mcp_tool_for_test(
            "docs",
            serde_json::json!({
                "name": "lookup",
                "title": "Top Level",
                "inputSchema": {"type": "object"},
                "annotations": {"title": "Annotated"}
            }),
        );
        assert_eq!(
            annotated.mcp_annotations.title.as_deref(),
            Some("Annotated")
        );
        assert!(!annotated.mcp_annotations.read_only_hint);
    }

    /// Every `mcp.tools` entry answers what CC's per-tool closures do
    /// (`client.ts:1786-1809,1972-1976`): `description()` untruncated,
    /// `prompt()` cut at MAX_MCP_DESCRIPTION_LENGTH, the three annotation
    /// methods, and `userFacingName()` from `annotations.title || tool.name`.
    #[test]
    fn fetched_mcp_tools_carry_the_server_tool_metadata() {
        let raw = "metadata ".repeat(MAX_MCP_DESCRIPTION_LENGTH + 1);
        let read = mcp_tool_for_test(
            "docs",
            serde_json::json!({
                "name": "read",
                "description": raw,
                "inputSchema": {"type": "object"},
                "annotations": {"title": "Read Docs", "readOnlyHint": true}
            }),
        );
        let drop = mcp_tool_for_test(
            "docs",
            serde_json::json!({
                "name": "drop",
                "inputSchema": {"type": "object"},
                "annotations": {"title": "", "destructiveHint": true, "openWorldHint": true}
            }),
        );
        let auth = crate::tools::mcp_auth_tool::create_mcp_auth_tool(
            "docs",
            &ScopedMcpServerConfig {
                name: None,
                scope: crate::services::mcp::types::ConfigScope::Local,
                transport: Transport::Http,
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                url: Some("https://mcp.example.com".to_string()),
                headers: std::collections::BTreeMap::new(),
                headers_helper: None,
                oauth: None,
                ide_running_in_windows: None,
                ide_name: None,
                auth_token: None,
                plugin_source: None,
            },
        );
        let authenticate = auth.tool();

        assert_eq!(read.mcp_description, raw);
        assert_eq!(read.description, truncate_mcp_description(Some(raw)));
        assert!(read.mcp_annotations.read_only_hint);
        assert!(!read.mcp_annotations.destructive_hint);
        assert_eq!(
            read.mcp_user_facing_name().as_deref(),
            Some("docs - Read Docs (MCP)")
        );

        assert_eq!(drop.mcp_description, "");
        assert!(!drop.mcp_annotations.read_only_hint);
        assert!(drop.mcp_annotations.destructive_hint);
        assert!(drop.mcp_annotations.open_world_hint);
        // `||` is JS truthiness: an empty title falls through to the name.
        assert_eq!(
            drop.mcp_user_facing_name().as_deref(),
            Some("docs - drop (MCP)")
        );

        // CC `McpAuthTool.ts:70`.
        assert_eq!(
            authenticate.mcp_user_facing_name().as_deref(),
            Some(auth.user_facing_name())
        );
    }
}
