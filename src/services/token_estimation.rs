//! Token estimation/counting service.
//!
//! Maps to: CC `services/tokenEstimation.ts`. This module owns the count-token
//! API boundary and reuses the canonical provider/auth client factory.

use crate::types::message::{AssistantContent, Message, UserContent};
use serde_json::Value;

async fn count_tokens_client(
    model: &str,
) -> Option<crate::services::provider::anthropic::client::AnthropicClientHandle> {
    // The binary installs this during startup; library/test callers can enter
    // this API boundary directly.
    crate::utils::tls_provider::install_crypto_provider();
    let options =
        crate::services::provider::anthropic::client::GetAnthropicClientOptions::for_profile(
            crate::bootstrap::state::get_session_profile().as_deref(),
            1,
            Some(model.to_string()),
            Some("count_tokens".to_string()),
        )
        .ok()?;
    crate::services::provider::anthropic::client::get_anthropic_client(options)
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

fn estimate_messages_tokens_rough(
    messages: &[anthropic_sdk::resources::beta::messages::BetaMessageParam],
) -> usize {
    let mut total = 0;
    for message in messages {
        match &message.content {
            anthropic_sdk::resources::beta::messages::BetaMessageContent::Text(text) => {
                total += rough_token_count_estimation(text);
            }
            anthropic_sdk::resources::beta::messages::BetaMessageContent::Blocks(blocks) => {
                for block in blocks {
                    if let Ok(val) = serde_json::to_value(block)
                        && let Some(text) = val.get("text").and_then(Value::as_str)
                    {
                        total += rough_token_count_estimation(text);
                    }
                }
            }
        }
    }
    total.max(0) as usize
}

/// Maps to: CC `services/tokenEstimation.ts#countTokensWithAPI` (:124-137).
pub async fn count_tokens_with_api(content: &str) -> Option<usize> {
    if content.is_empty() {
        return Some(0);
    }
    let settings = crate::utils::settings::get_initial_settings();
    let profile = crate::utils::model::profile::resolve_request_profile(
        &settings,
        crate::bootstrap::state::get_session_profile().as_deref(),
    )
    .ok();
    if let Some(profile) = &profile {
        match profile.protocol {
            crate::utils::settings::nyan::ProviderProtocol::Gemini => {
                match crate::services::provider::gemini::count_tokens(profile, content).await {
                    Ok(count) => return Some(count),
                    Err(error) => {
                        tracing::warn!(error = %error, "Gemini count_tokens failed, falling back to local estimate");
                        return Some(rough_token_count_estimation(content).max(0) as usize);
                    }
                }
            }
            crate::utils::settings::nyan::ProviderProtocol::OpenaiChat
            | crate::utils::settings::nyan::ProviderProtocol::OpenaiResponses => {
                return Some(rough_token_count_estimation(content).max(0) as usize);
            }
            crate::utils::settings::nyan::ProviderProtocol::AnthropicMessages => {}
        }
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
    let settings = crate::utils::settings::get_initial_settings();
    let profile = crate::utils::model::profile::resolve_request_profile(
        &settings,
        crate::bootstrap::state::get_session_profile().as_deref(),
    )
    .ok();
    if let Some(profile) = &profile {
        match profile.protocol {
            crate::utils::settings::nyan::ProviderProtocol::Gemini => {
                match crate::services::provider::gemini::count_messages_tokens(profile, &messages)
                    .await
                {
                    Ok(count) => return Some(count),
                    Err(error) => {
                        tracing::warn!(error = %error, "Gemini count_tokens failed, falling back to local estimate");
                        return Some(estimate_messages_tokens_rough(&messages));
                    }
                }
            }
            crate::utils::settings::nyan::ProviderProtocol::OpenaiChat
            | crate::utils::settings::nyan::ProviderProtocol::OpenaiResponses => {
                return Some(estimate_messages_tokens_rough(&messages));
            }
            crate::utils::settings::nyan::ProviderProtocol::AnthropicMessages => {}
        }
    }

    let model = crate::utils::model::model::get_main_loop_model();
    if model.trim().is_empty() {
        return None;
    }
    let normalized_model = crate::utils::model::model::normalize_model_string_for_api(&model);
    let contains_thinking = has_thinking_blocks(&messages);
    let betas = crate::services::provider::anthropic::betas::get_model_betas(&model);
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

    /// Reads one HTTP/1.1 request, head and body.
    fn read_request(stream: &mut std::net::TcpStream) -> String {
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
        String::from_utf8(request).unwrap()
    }

    fn write_json_response(stream: &mut std::net::TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    }

    fn spawn_json_server(response_body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            write_json_response(&mut stream, response_body);
            request
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

    /// Count-token calls need a selected main profile. The cloud variants take
    /// their endpoint and credentials from their own variables, so the profile
    /// here only has to exist; it is removed again when the guard drops.
    struct MainProfileGuard {
        _key: EnvVarGuard,
    }

    impl MainProfileGuard {
        fn install() -> Self {
            let key = EnvVarGuard::set("NYAN_TOKEN_TEST_KEY", "sk-unused-by-cloud-variants");
            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::services::provider::anthropic::target::install_test_main_profile(
                "https://unused.invalid",
                "NYAN_TOKEN_TEST_KEY",
            );
            Self { _key: key }
        }
    }

    impl Drop for MainProfileGuard {
        fn drop(&mut self) {
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }
}
