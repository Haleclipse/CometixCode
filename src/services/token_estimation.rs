//! Token estimation/counting service.
//!
//! Maps to: CC `services/tokenEstimation.ts`. This module owns the count-token
//! API boundary and reuses the canonical provider/auth client factory.

use crate::types::message::{AssistantContent, Message, UserContent};
use crate::utils::process_env::JsTruthy;
use base64::Engine as _;
use serde_json::Value;

async fn count_tokens_client(
    model: &str,
) -> Option<crate::services::api::client::AnthropicClientHandle> {
    // The binary installs this during startup; library/test callers can enter
    // this API boundary directly.
    crate::utils::tls_provider::install_crypto_provider();
    crate::services::api::client::get_anthropic_client(
        crate::services::api::client::GetAnthropicClientOptions {
            max_retries: 1,
            model: Some(model.to_string()),
            source: Some("count_tokens".to_string()),
            ..Default::default()
        },
    )
    .await
    .ok()
}

/// Maps to: CC `services/tokenEstimation.ts#hasThinkingBlocks`.
fn has_thinking_blocks(
    messages: &[anthropic_sdk::resources::beta::messages::BetaMessageParam],
) -> bool {
    messages.iter().any(|message| {
        if message.role != "assistant" {
            return false;
        }
        serde_json::to_value(message)
            .ok()
            .and_then(|value| value.get("content").and_then(Value::as_array).cloned())
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
                })
            })
    })
}

/// Maps to: CC `services/tokenEstimation.ts#countTokensWithBedrock`.
async fn count_tokens_with_bedrock(
    handle: &crate::services::api::client::AnthropicClientHandle,
    model: &str,
    messages: &[anthropic_sdk::resources::beta::messages::BetaMessageParam],
    tools: &[anthropic_sdk::resources::beta::messages::BetaToolUnion],
    betas: &[String],
    contains_thinking: bool,
) -> Option<usize> {
    let crate::services::api::client::ProviderConfig::Bedrock { region, auth } = &handle.provider
    else {
        return None;
    };
    let model_id = if crate::utils::model::bedrock::is_foundation_model(model) {
        model.to_string()
    } else {
        crate::utils::model::bedrock::get_inference_profile_backing_model(model, region, auth)
            .await?
    };
    let mut invoke_body = serde_json::json!({
        "anthropic_version": "bedrock-2023-05-31",
        "messages": if messages.is_empty() {
            serde_json::json!([{"role": "user", "content": "foo"}])
        } else {
            serde_json::to_value(messages).ok()?
        },
        "max_tokens": if contains_thinking { 2048 } else { 1 },
    });
    if !tools.is_empty() {
        invoke_body["tools"] = serde_json::to_value(tools).ok()?;
    }
    if !betas.is_empty() {
        invoke_body["anthropic_beta"] = serde_json::to_value(betas).ok()?;
    }
    if contains_thinking {
        invoke_body["thinking"] = serde_json::json!({
            "type": "enabled",
            "budget_tokens": 1024
        });
    }
    let encoded_body =
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&invoke_body).ok()?);
    let request_body = serde_json::to_vec(&serde_json::json!({
        "input": {"invokeModel": {"body": encoded_body}}
    }))
    .ok()?;
    let endpoint = crate::utils::process_env::var("ANTHROPIC_BEDROCK_BASE_URL")
        .truthy()
        .or_else(|| crate::utils::process_env::var("AWS_ENDPOINT_URL_BEDROCK_RUNTIME").truthy())
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"));
    let response = crate::services::api::client::send_bedrock_request(
        reqwest::Method::POST,
        &endpoint,
        &format!(
            "/model/{}/count-tokens",
            crate::services::api::client::aws_uri_encode(&model_id)
        ),
        request_body,
        region,
        "bedrock",
        auth,
    )
    .await?;
    response
        .get("inputTokens")
        .and_then(Value::as_u64)
        .and_then(|tokens| usize::try_from(tokens).ok())
}

/// Maps to: CC `services/tokenEstimation.ts:203-208` `roughTokenCountEstimation`
/// with the source's default `bytesPerToken = 4`.
pub fn rough_token_count_estimation(content: &str) -> i64 {
    ((content.encode_utf16().count() + 2) / 4) as i64
}

/// Maps to: CC `services/tokenEstimation.ts#bytesPerTokenForFileType`.
pub fn bytes_per_token_for_file_type(file_extension: &str) -> usize {
    match file_extension {
        "json" | "jsonl" | "jsonc" => 2,
        _ => 4,
    }
}

/// Maps to: CC `services/tokenEstimation.ts#roughTokenCountEstimationForFileType`.
pub fn rough_token_count_estimation_for_file_type(content: &str, file_extension: &str) -> usize {
    let bytes_per_token = bytes_per_token_for_file_type(file_extension);
    (content.encode_utf16().count() + bytes_per_token / 2) / bytes_per_token
}

/// Conservative image/document token estimate shared with CC
/// `services/compact/microCompact.ts` `IMAGE_MAX_TOKEN_SIZE` and
/// `services/tokenEstimation.ts` image/document handling.
const IMAGE_MAX_TOKEN_SIZE: i64 = 2_000;

/// L1 typed-content carrier: CC's union of string/block arrays ≙ Rust's
/// separate strongly typed user/assistant block slices.
enum RoughTokenContent<'a> {
    User(&'a [UserContent]),
    Assistant(&'a [AssistantContent]),
}

/// L1 typed-block carrier for CC's `ContentBlock | ContentBlockParam` union.
enum RoughTokenBlock<'a> {
    User(&'a UserContent),
    Assistant(&'a AssistantContent),
}

/// Maps to: CC `services/tokenEstimation.ts:327-339`
/// `roughTokenCountEstimationForMessages`. This mirrors the official block-wise
/// shape instead of flattening Rust-only `toolUseResult` fields into one character count.
pub fn rough_token_count_estimation_for_messages(messages: &[Message]) -> i64 {
    messages
        .iter()
        .map(rough_token_count_estimation_for_message)
        .sum()
}

/// Maps to: CC `services/tokenEstimation.ts:341-369`
/// `roughTokenCountEstimationForMessage`.
fn rough_token_count_estimation_for_message(message: &Message) -> i64 {
    match message {
        // CC :368: everything except user/assistant/attachment returns 0.
        Message::Progress(_) => 0,
        Message::User(user) => {
            rough_token_count_estimation_for_content(RoughTokenContent::User(&user.content))
        }
        Message::Assistant(assistant) => rough_token_count_estimation_for_content(
            RoughTokenContent::Assistant(&assistant.content),
        ),
        // Official estimator ignores local system rows; API normalization
        // filters compact-boundary/system UI messages before model calls.
        Message::System(_) => 0,
        // Maps to CC `roughTokenCountEstimationForMessage`: attachment token
        // cost is the normalized API-visible user content, not raw JSONL bytes.
        Message::Attachment(attachment) => {
            // Use a non-exempt model for conservative, I/O-free attachment
            // estimation; the request path supplies its actual model.
            crate::utils::messages::normalize_attachment_for_api(
                attachment,
                Some("claude-sonnet-4-6"),
            )
            .iter()
            .map(|user| {
                rough_token_count_estimation_for_content(RoughTokenContent::User(&user.content))
            })
            .sum()
        }
        Message::HookResult(hook)
            if hook
                .attachment
                .get("type")
                .and_then(serde_json::Value::as_str)
                == Some("hook_additional_context") =>
        {
            hook.attachment
                .get("content")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(rough_token_count_estimation)
                .sum()
        }
        Message::HookResult(_) => 0,
    }
}

/// Maps to: CC `services/tokenEstimation.ts:371-389`
/// `roughTokenCountEstimationForContent`.
fn rough_token_count_estimation_for_content(content: RoughTokenContent<'_>) -> i64 {
    match content {
        RoughTokenContent::User(blocks) => blocks
            .iter()
            .map(|block| rough_token_count_estimation_for_block(RoughTokenBlock::User(block)))
            .sum(),
        RoughTokenContent::Assistant(blocks) => blocks
            .iter()
            .map(|block| rough_token_count_estimation_for_block(RoughTokenBlock::Assistant(block)))
            .sum(),
    }
}

/// Maps to: CC `services/tokenEstimation.ts:391-435`
/// `roughTokenCountEstimationForBlock`.
fn rough_token_count_estimation_for_block(block: RoughTokenBlock<'_>) -> i64 {
    match block {
        RoughTokenBlock::User(content) => match content {
            UserContent::Text(text) | UserContent::MetaText(text) => {
                rough_token_count_estimation(text)
            }
            UserContent::Image { .. }
            | UserContent::MetaImage { .. }
            | UserContent::RawImage { .. } => IMAGE_MAX_TOKEN_SIZE,
            UserContent::Document { .. } | UserContent::MetaDocument { .. } => IMAGE_MAX_TOKEN_SIZE,
            UserContent::ToolResult(result) => {
                if result.content_blocks.is_empty() {
                    rough_token_count_estimation(&result.content)
                } else {
                    result
                        .content_blocks
                        .iter()
                        .map(|block| match block {
                            crate::types::message::ToolResultContentBlock::ToolReference {
                                ..
                            } => rough_token_count_estimation(
                                &serde_json::to_string(block).unwrap_or_default(),
                            ),
                            crate::types::message::ToolResultContentBlock::Text { text } => {
                                rough_token_count_estimation(text)
                            }
                            crate::types::message::ToolResultContentBlock::Image { .. }
                            | crate::types::message::ToolResultContentBlock::Document { .. }
                            | crate::types::message::ToolResultContentBlock::RawImage(_) => {
                                IMAGE_MAX_TOKEN_SIZE
                            }
                        })
                        .sum()
                }
            }
        },
        RoughTokenBlock::Assistant(content) => match content {
            AssistantContent::Text(text) => rough_token_count_estimation(text),
            AssistantContent::Thinking { text, .. } => rough_token_count_estimation(text),
            AssistantContent::RedactedThinking { data } => rough_token_count_estimation(data),
            AssistantContent::ToolUse(tool_use) => {
                rough_token_count_estimation(&format!("{}{}", tool_use.name, tool_use.input))
            }
            AssistantContent::ServerToolUse(tool_use) => rough_token_count_estimation(
                &serde_json::json!({
                    "type": "server_tool_use",
                    "id": &tool_use.id.0,
                    "name": &tool_use.name,
                    "input": &tool_use.input,
                })
                .to_string(),
            ),
            AssistantContent::WebSearchToolResult {
                tool_use_id,
                content,
            } => rough_token_count_estimation(
                &serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": &tool_use_id.0,
                    "content": content,
                })
                .to_string(),
            ),
            // CC has no advisor branch: the block falls to the catch-all for
            // server-side payloads, `roughTokenCountEstimation(jsonStringify(block))`
            // (tokenEstimation.ts:430-434), same as web_search_tool_result
            // above. Estimating only `text()` billed the envelope at zero and,
            // worse, billed a redacted result at zero outright — its
            // `encrypted_content` does reach the API, so the context-budget
            // check would think the turn was cheaper than it is.
            AssistantContent::Advisor {
                tool_use_id,
                content,
            } => rough_token_count_estimation(
                &serde_json::json!({
                    "type": "advisor_tool_result",
                    "tool_use_id": &tool_use_id.0,
                    "content": content,
                })
                .to_string(),
            ),
            AssistantContent::MessageIdentity(_) => 0,
        },
    }
}

/// Maps to: CC `services/tokenEstimation.ts#countTokensWithAPI` (:124-137).
pub async fn count_tokens_with_api(content: &str) -> Option<usize> {
    if content.is_empty() {
        return Some(0);
    }
    let message = anthropic_sdk::resources::beta::messages::BetaMessageParam {
        role: "user".to_string(),
        content: anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(
            content.to_string(),
        ),
    };
    count_messages_tokens_with_api(vec![message], Vec::new()).await
}

/// Maps to: CC `services/tokenEstimation.ts#countMessagesTokensWithAPI`.
pub async fn count_messages_tokens_with_api(
    messages: Vec<anthropic_sdk::resources::beta::messages::BetaMessageParam>,
    tools: Vec<anthropic_sdk::resources::beta::messages::BetaToolUnion>,
) -> Option<usize> {
    let model = crate::utils::model::model::get_main_loop_model();
    let normalized_model = crate::utils::model::model::normalize_model_string_for_api(&model);
    let provider = crate::utils::model::providers::get_api_provider();
    let contains_thinking = has_thinking_blocks(&messages);
    let mut betas = crate::utils::betas::get_model_betas(&model);
    if provider == crate::utils::model::providers::ApiProvider::Vertex {
        betas.retain(|beta| {
            matches!(
                beta.as_str(),
                "claude-code-20250219"
                    | "interleaved-thinking-2025-05-14"
                    | "context-management-2025-06-27"
            )
        });
    }
    let messages = if messages.is_empty() {
        vec![anthropic_sdk::resources::beta::messages::BetaMessageParam {
            role: "user".to_string(),
            content: anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(
                "foo".to_string(),
            ),
        }]
    } else {
        messages
    };
    let params = anthropic_sdk::resources::beta::messages::BetaMessageCountTokensParams {
        messages: messages.clone(),
        model: normalized_model.clone(),
        betas: (!betas.is_empty()).then(|| betas.clone()),
        thinking: contains_thinking.then_some(
            anthropic_sdk::resources::messages::ThinkingConfig::Enabled {
                budget_tokens: 1024,
            },
        ),
        tools: Some(tools.clone()),
        ..Default::default()
    };
    let handle = count_tokens_client(&model).await?;

    match provider {
        crate::utils::model::providers::ApiProvider::Bedrock => {
            count_tokens_with_bedrock(
                &handle,
                &normalized_model,
                &messages,
                &tools,
                &betas,
                contains_thinking,
            )
            .await
        }
        // CC `anthropic.beta.messages.countTokens` (`tokenEstimation.ts:172`),
        // through the provider client; Vertex's betas are filtered above.
        crate::utils::model::providers::ApiProvider::FirstParty
        | crate::utils::model::providers::ApiProvider::Foundry
        | crate::utils::model::providers::ApiProvider::Vertex => {
            let client = handle.build().ok()?;
            match client.beta().messages().count_tokens(&params).await {
                Ok(response) if response.input_tokens >= 0 => Some(response.input_tokens as usize),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(error = %error, "count_tokens API request failed");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::io::{Read as _, Write as _};

    /// Maps to: CC `tokenEstimation.ts:430-434` — advisor blocks hit the
    /// server-side catch-all, `roughTokenCountEstimation(jsonStringify(block))`.
    /// A redacted result carries its payload in `encrypted_content`, which
    /// does reach the API; estimating only `text()` scored it zero and made
    /// the turn look free to the context-budget check.
    #[test]
    fn redacted_advisor_result_is_not_estimated_as_free() {
        let redacted = AssistantContent::Advisor {
            tool_use_id: crate::types::ids::ToolUseId("srvtoolu_advisor".to_string()),
            content: crate::types::message::AdvisorResult::Redacted {
                encrypted_content: "A".repeat(400),
            },
        };

        let estimate =
            rough_token_count_estimation_for_block(RoughTokenBlock::Assistant(&redacted));

        assert!(
            estimate > 50,
            "400 chars of encrypted payload must not estimate near zero, got {estimate}"
        );
    }

    /// The envelope counts too: CC stringifies the whole block, so `type` and
    /// `tool_use_id` are part of what the API sees.
    #[test]
    fn advisor_result_estimate_includes_the_block_envelope() {
        let advisor = AssistantContent::Advisor {
            tool_use_id: crate::types::ids::ToolUseId("srvtoolu_advisor".to_string()),
            content: crate::types::message::AdvisorResult::Result {
                text: "advice".to_string(),
            },
        };

        let whole = rough_token_count_estimation_for_block(RoughTokenBlock::Assistant(&advisor));
        let text_only = rough_token_count_estimation("advice");

        assert!(
            whole > text_only,
            "envelope keys are part of the serialized form ({whole} vs {text_only})"
        );
    }

    fn spawn_json_server(response_body: &'static str) -> (String, std::thread::JoinHandle<String>) {
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
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}"), worker)
    }

    #[test]
    fn rough_file_type_estimation_matches_json_ratio_and_utf16_length() {
        assert_eq!(rough_token_count_estimation_for_file_type("1234", "txt"), 1);
        assert_eq!(
            rough_token_count_estimation_for_file_type("1234", "json"),
            2
        );
        assert_eq!(rough_token_count_estimation_for_file_type("😀😀", "txt"), 1);
    }

    #[tokio::test]
    async fn bedrock_count_tokens_uses_runtime_count_tokens_request() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let (endpoint, server) = spawn_json_server(r#"{"inputTokens":23}"#);
        let _bedrock = EnvVarGuard::set("CLAUDE_CODE_USE_BEDROCK", "1");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _skip_auth = EnvVarGuard::set("CLAUDE_CODE_SKIP_BEDROCK_AUTH", "1");
        let _endpoint = EnvVarGuard::set("AWS_ENDPOINT_URL_BEDROCK_RUNTIME", &endpoint);
        let _model = EnvVarGuard::set(
            "ANTHROPIC_MODEL",
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
        );

        assert_eq!(count_tokens_with_api("hello").await, Some(23));
        let request = server.join().unwrap();
        assert!(
            request.starts_with(
                "POST /model/anthropic.claude-sonnet-4-5-20250929-v1%3A0/count-tokens "
            )
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        let encoded = body["input"]["invokeModel"]["body"].as_str().unwrap();
        let invoke: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(invoke["anthropic_version"], "bedrock-2023-05-31");
        assert_eq!(invoke["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn vertex_count_tokens_uses_raw_predict_route() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let (endpoint, server) = spawn_json_server(r#"{"input_tokens":19}"#);
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::set("CLAUDE_CODE_USE_VERTEX", "1");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let _skip_auth = EnvVarGuard::set("CLAUDE_CODE_SKIP_VERTEX_AUTH", "1");
        let _endpoint = EnvVarGuard::set("ANTHROPIC_VERTEX_BASE_URL", &endpoint);
        let _project = EnvVarGuard::set("ANTHROPIC_VERTEX_PROJECT_ID", "project-a");
        let _region = EnvVarGuard::set("CLOUD_ML_REGION", "us-east5");
        let _model = EnvVarGuard::set("ANTHROPIC_MODEL", "claude-sonnet-4-5");

        assert_eq!(count_tokens_with_api("hello").await, Some(19));
        let request = server.join().unwrap();
        assert!(request.starts_with(
            "POST /projects/project-a/locations/us-east5/publishers/anthropic/models/count-tokens:rawPredict "
        ));
        assert!(!request.contains("web-search-2025-03-05"));
        assert!(request.contains("claude-code-20250219"));
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn foundry_count_tokens_uses_configured_anthropic_client() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let (endpoint, server) = spawn_json_server(r#"{"input_tokens":17}"#);
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::set("CLAUDE_CODE_USE_FOUNDRY", "1");
        let _base_url = EnvVarGuard::set(
            "ANTHROPIC_FOUNDRY_BASE_URL",
            &format!("{endpoint}/anthropic/"),
        );
        let _api_key = EnvVarGuard::set("ANTHROPIC_FOUNDRY_API_KEY", "foundry-test-key");
        let _model = EnvVarGuard::set("ANTHROPIC_MODEL", "claude-sonnet-4-5");

        assert_eq!(count_tokens_with_api("hello").await, Some(17));
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /anthropic/v1/messages/count_tokens?beta=true "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-api-key: foundry-test-key")
        );
    }
}
