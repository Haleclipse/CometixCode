//! Token accounting helpers.
//! Maps to CC `utils/tokens.ts`.
//!
//! The official implementation prefers usage from the most recent real API
//! response and estimates only messages added after it. Nyan's typed message
//! model does not yet carry Anthropic response IDs for split assistant records,
//! so this port keeps the same ownership boundary and uses the last usage-bearing
//! assistant as the anchor.

use crate::services::token_estimation::{
    rough_token_count_estimation, rough_token_count_estimation_for_messages,
};
use crate::types::message::{AssistantContent, AssistantMessage, Message, TokenUsage, UserContent};

/// Maps to CC `utils/tokens.ts` `getTokenUsage(...)`.
///
/// CC filters synthetic assistant rows here. Nyan's typed model-message
/// layer does not carry synthetic assistant sentinels, so every usage-bearing
/// assistant message is treated as a real API response.
fn get_token_usage(message: &Message) -> Option<&TokenUsage> {
    match message {
        Message::Assistant(assistant) => assistant.usage.as_ref(),
        _ => None,
    }
}

/// Maps to CC `utils/tokens.ts` `getTokenCountFromUsage(...)`.
pub fn get_token_count_from_usage(usage: &TokenUsage) -> i64 {
    (usage.input_tokens
        + usage.cache_creation_input_tokens
        + usage.cache_read_input_tokens
        + usage.output_tokens) as i64
}

/// Maps to CC `utils/tokens.ts` `tokenCountFromLastAPIResponse(...)`.
pub fn token_count_from_last_api_response(messages: &[Message]) -> i64 {
    messages
        .iter()
        .rev()
        .find_map(get_token_usage)
        .map(get_token_count_from_usage)
        .unwrap_or(0)
}

/// Maps to CC `utils/tokens.ts` `finalContextTokensFromLastResponse(...)`.
///
/// Rust `TokenUsage` does not yet model the API `usage.iterations` array, so
/// this implements the official fallback branch: top-level `input_tokens +
/// output_tokens` and deliberately excludes cache tokens.
pub fn final_context_tokens_from_last_response(messages: &[Message]) -> u64 {
    messages
        .iter()
        .rev()
        .find_map(get_token_usage)
        .map(|usage| usage.input_tokens.saturating_add(usage.output_tokens))
        .unwrap_or(0)
}

/// Maps to CC `utils/tokens.ts` `messageTokenCountFromLastAPIResponse(...)`.
pub fn message_token_count_from_last_api_response(messages: &[Message]) -> u64 {
    messages
        .iter()
        .rev()
        .find_map(get_token_usage)
        .map(|usage| usage.output_tokens)
        .unwrap_or(0)
}

/// Maps to CC `utils/tokens.ts` `getCurrentUsage(...)`.
pub fn get_current_usage(messages: &[Message]) -> Option<TokenUsage> {
    messages.iter().rev().find_map(get_token_usage).cloned()
}

/// Maps to CC `utils/tokens.ts` `doesMostRecentAssistantMessageExceed200k(...)`.
pub fn does_most_recent_assistant_message_exceed_200k(messages: &[Message]) -> bool {
    const THRESHOLD: i64 = 200_000;
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .and_then(|assistant| assistant.usage.as_ref())
        .is_some_and(|usage| get_token_count_from_usage(usage) > THRESHOLD)
}

/// Maps to CC `utils/tokens.ts` `getAssistantMessageContentLength(...)`.
pub fn get_assistant_message_content_length(message: &AssistantMessage) -> usize {
    message
        .content
        .iter()
        .map(|content| match content {
            AssistantContent::Text(text) => text.len(),
            AssistantContent::Thinking { text, .. } => text.len(),
            AssistantContent::RedactedThinking { data } => data.len(),
            AssistantContent::ToolUse(tool_use) => tool_use.input.to_string().len(),
            AssistantContent::ServerToolUse(_)
            | AssistantContent::WebSearchToolResult { .. }
            | AssistantContent::Advisor { .. }
            | AssistantContent::MessageIdentity(_) => 0,
        })
        .sum()
}

/// Maps to CC `utils/tokens.ts` `tokenCountWithEstimation(...)`.
pub fn token_count_with_estimation(messages: &[Message]) -> i64 {
    let mut anchor: Option<(usize, &TokenUsage)> = None;
    for (index, message) in messages.iter().enumerate().rev() {
        if let Message::Assistant(assistant) = message
            && let Some(usage) = assistant.usage.as_ref()
        {
            anchor = Some((index, usage));
            break;
        }
    }

    if let Some((index, usage)) = anchor {
        get_token_count_from_usage(usage)
            + rough_token_count_estimation_for_messages(&messages[index + 1..])
    } else {
        rough_token_count_estimation_for_messages(messages)
    }
}

/// @nyan: index of the assistant message whose `usage` anchors
/// [`token_count_with_estimation`]. Everything at or before it is already
/// counted inside that usage, so a replacement decided after the request was
/// sent has to be subtracted explicitly (compaction C4).
pub fn usage_anchor_index(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(
        |message| matches!(message, Message::Assistant(assistant) if assistant.usage.is_some()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_text(text: impl Into<String>) -> Message {
        Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(text.into())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    #[test]
    fn final_context_tokens_from_last_response_uses_input_plus_output_without_cache() {
        let messages = vec![Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("response".to_string())],
            model: Some("claude".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 25,
                cache_creation_input_tokens: 500,
                cache_read_input_tokens: 700,
                ..Default::default()
            }),
        })];

        assert_eq!(final_context_tokens_from_last_response(&messages), 125);
        assert_eq!(message_token_count_from_last_api_response(&messages), 25);
        assert_eq!(
            get_current_usage(&messages)
                .unwrap()
                .cache_read_input_tokens,
            700
        );
    }

    #[test]
    fn most_recent_assistant_200k_check_matches_official_find_last_assistant_semantics() {
        let older_large = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("large".to_string())],
            model: Some("claude".to_string()),
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: Some(TokenUsage {
                input_tokens: 200_000,
                output_tokens: 2,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                ..Default::default()
            }),
        });
        let newer_without_usage = Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text("synthetic-ish".to_string())],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        });
        let user_after = user_text("after");

        assert!(does_most_recent_assistant_message_exceed_200k(&[
            older_large.clone(),
            user_after.clone(),
        ]));
        assert!(!does_most_recent_assistant_message_exceed_200k(&[
            older_large,
            newer_without_usage,
            user_after,
        ]));
    }

    #[test]
    fn rough_token_count_estimation_uses_utf16_code_units_like_javascript() {
        assert_eq!(rough_token_count_estimation("éééééééé"), 2);
    }

    #[test]
    fn token_count_with_estimation_uses_last_usage_plus_new_messages() {
        let messages = vec![
            user_text("before"),
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("response".to_string())],
                model: Some("claude-sonnet-4-20250514".to_string()),
                stop_reason: Some(crate::types::message::StopReason::EndTurn),
                usage: Some(TokenUsage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_creation_input_tokens: 3,
                    cache_read_input_tokens: 7,
                    ..Default::default()
                }),
            }),
            user_text("xxxxxxxx"),
        ];

        assert_eq!(token_count_from_last_api_response(&messages), 130);
        assert_eq!(token_count_with_estimation(&messages), 132);
    }

    #[test]
    fn assistant_content_length_counts_tool_use_input_without_name_like_official() {
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_1".to_string()),
                    name: "Read".to_string(),
                    input: serde_json::json!({"file_path":"Cargo.toml"}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        assert_eq!(
            get_assistant_message_content_length(&assistant),
            serde_json::json!({"file_path":"Cargo.toml"})
                .to_string()
                .len()
        );
    }

    #[test]
    fn assistant_content_length_ignores_server_tool_blocks_like_official() {
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::ServerToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("srvu_1".to_string()),
                    name: "web_search".to_string(),
                    input: serde_json::json!({"query":"rust"}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        };

        assert_eq!(get_assistant_message_content_length(&assistant), 0);
    }

    #[test]
    fn assistant_content_length_counts_redacted_thinking_data_like_official() {
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::RedactedThinking {
                data: "abcdef".to_string(),
            }],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::EndTurn),
            usage: None,
        };

        assert_eq!(get_assistant_message_content_length(&assistant), 6);
    }

    #[test]
    fn token_count_with_estimation_falls_back_to_rough_message_length() {
        let messages = vec![user_text("abcdefgh"), user_text("ijkl")];
        assert_eq!(token_count_with_estimation(&messages), 3);
    }

    #[test]
    fn rough_token_estimation_counts_image_blocks_like_official_fixed_cost() {
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Image {
                media_type: "image/png".to_string(),
                data: "a".repeat(64_000),
            }],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];

        assert_eq!(rough_token_count_estimation_for_messages(&messages), 2_000);
    }

    #[test]
    fn rough_token_estimation_counts_tool_reference_wire_blocks_not_raw_content() {
        let block = crate::types::message::ToolResultContentBlock::ToolReference {
            tool_name: "Read".to_string(),
        };
        let expected = rough_token_count_estimation(
            &serde_json::to_string(&block).expect("tool_reference serializes"),
        );
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(crate::types::message::ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_ref".to_string()),
                content: "raw content should not be sent when blocks exist".repeat(100),
                is_error: false,
                content_blocks: vec![block],
                tool_use_result: None,
            })],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];

        assert_eq!(
            rough_token_count_estimation_for_messages(&messages),
            expected
        );
    }
}
