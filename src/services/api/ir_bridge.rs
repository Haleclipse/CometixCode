//! Query layer <-> provider IR stream events.
//!
//! @nyan: the query layer consumes [`StreamEventKind`] from whichever adapter
//! served the request. This module holds the three conversions it needs and
//! nothing provider-specific: IR usage / stop reason to the transcript types,
//! and an IR event to the `ApiEvent` JSON envelope that the REPL, compaction,
//! forked agents and hooks already read (`type`, `delta.text`, `usage`,
//! `request_id`). The envelope shape is the app's own stream vocabulary, not a
//! wire format: it is rendered from the neutral event, never copied from a
//! provider payload, so every adapter produces the same one.

use serde_json::{Value, json};

use crate::services::provider::ir::{BlockStart, Delta, StopReason, StreamEventKind, Usage};
use crate::types::message::{StopReason as MessageStopReason, TokenUsage};

pub fn token_usage(usage: &Usage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_write_tokens,
        cache_read_input_tokens: usage.cache_read_tokens,
        cache_deleted_input_tokens: usage.cache_deleted_tokens,
    }
}

/// `Other` has no transcript counterpart; the message then carries no stop
/// reason, which the query loop already treats as "unknown".
pub fn message_stop_reason(reason: &StopReason) -> Option<MessageStopReason> {
    match reason {
        StopReason::EndTurn => Some(MessageStopReason::EndTurn),
        StopReason::ToolUse => Some(MessageStopReason::ToolUse),
        StopReason::MaxTokens => Some(MessageStopReason::MaxTokens),
        StopReason::StopSequence => Some(MessageStopReason::StopSequence),
        StopReason::PauseTurn => Some(MessageStopReason::PauseTurn),
        StopReason::Refusal => Some(MessageStopReason::Refusal),
        StopReason::Other(_) => None,
    }
}

/// Short event name for trace output.
pub fn event_label(kind: &StreamEventKind) -> &'static str {
    match kind {
        StreamEventKind::MessageStart { .. } => "message_start",
        StreamEventKind::BlockStart { .. } => "content_block_start",
        StreamEventKind::BlockDelta { .. } => "content_block_delta",
        StreamEventKind::BlockStop { .. } => "content_block_stop",
        StreamEventKind::MessageDelta { .. } => "message_delta",
        StreamEventKind::MessageStop => "message_stop",
    }
}

fn usage_json(usage: &Usage) -> Value {
    let mut object = json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
    });
    if usage.cache_deleted_tokens > 0 {
        object["cache_deleted_input_tokens"] = json!(usage.cache_deleted_tokens);
    }
    object
}

fn stop_reason_str(reason: &StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::ToolUse => "tool_use".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::StopSequence => "stop_sequence".to_string(),
        StopReason::PauseTurn => "pause_turn".to_string(),
        StopReason::Refusal => "refusal".to_string(),
        StopReason::Other(other) => other.clone(),
    }
}

fn block_type(block: &BlockStart) -> &'static str {
    match block {
        BlockStart::Text => "text",
        BlockStart::Thinking => "thinking",
        BlockStart::RedactedThinking { .. } => "redacted_thinking",
        BlockStart::ToolCall { server: false, .. } => "tool_use",
        BlockStart::ToolCall { server: true, .. } => "server_tool_use",
        BlockStart::ServerToolResult { .. } => "web_search_tool_result",
    }
}

/// The `ApiEvent` envelope for one IR event.
pub fn api_event_json(kind: &StreamEventKind) -> Value {
    match kind {
        StreamEventKind::MessageStart {
            request_id,
            message_id,
            model,
            usage,
        } => {
            let mut event = json!({
                "type": "message_start",
                "message": {
                    "id": message_id,
                    "model": model,
                    "usage": usage_json(usage),
                },
            });
            if let Some(request_id) = request_id {
                event["request_id"] = json!(request_id);
            }
            event
        }
        StreamEventKind::BlockStart { index, block } => json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": block_type(block) },
        }),
        StreamEventKind::BlockDelta { index, delta } => {
            let delta = match delta {
                Delta::Text { text } => json!({ "type": "text_delta", "text": text }),
                Delta::Thinking { text } => json!({ "type": "thinking_delta", "thinking": text }),
                Delta::Signature { signature } => {
                    json!({ "type": "signature_delta", "signature": signature })
                }
                Delta::ToolInput { json } => {
                    json!({ "type": "input_json_delta", "partial_json": json })
                }
            };
            json!({ "type": "content_block_delta", "index": index, "delta": delta })
        }
        StreamEventKind::BlockStop { index } => {
            json!({ "type": "content_block_stop", "index": index })
        }
        StreamEventKind::MessageDelta { stop_reason, usage } => json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason.as_ref().map(stop_reason_str) },
            "usage": usage_json(usage),
        }),
        StreamEventKind::MessageStop => json!({ "type": "message_stop" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_maps_cache_fields_and_keeps_deleted_tokens_only_when_present() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_write_tokens: 7,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 40,
            cache_deleted_tokens: 0,
            reasoning_tokens: 3,
        };
        let mapped = token_usage(&usage);
        assert_eq!(mapped.cache_creation_input_tokens, 7);
        assert_eq!(mapped.cache_read_input_tokens, 40);
        assert!(
            usage_json(&usage)
                .get("cache_deleted_input_tokens")
                .is_none()
        );
        let deleted = Usage {
            cache_deleted_tokens: 9,
            ..usage
        };
        assert_eq!(usage_json(&deleted)["cache_deleted_input_tokens"], 9);
        assert_eq!(token_usage(&deleted).cache_deleted_input_tokens, 9);
    }

    #[test]
    fn unknown_stop_reason_has_no_transcript_counterpart() {
        assert_eq!(
            message_stop_reason(&StopReason::ToolUse),
            Some(MessageStopReason::ToolUse)
        );
        assert_eq!(message_stop_reason(&StopReason::Other("x".into())), None);
    }

    #[test]
    fn envelope_carries_what_the_stream_consumers_read() {
        let start = api_event_json(&StreamEventKind::MessageStart {
            request_id: Some("req_1".into()),
            message_id: Some("msg_1".into()),
            model: Some("m".into()),
            usage: Usage::default(),
        });
        assert_eq!(start["type"], "message_start");
        assert_eq!(start["request_id"], "req_1");

        let text = api_event_json(&StreamEventKind::BlockDelta {
            index: 0,
            delta: Delta::Text { text: "hi".into() },
        });
        assert_eq!(text["delta"]["type"], "text_delta");
        assert_eq!(text["delta"]["text"], "hi");

        let tool = api_event_json(&StreamEventKind::BlockDelta {
            index: 1,
            delta: Delta::ToolInput {
                json: "{\"a\"".into(),
            },
        });
        assert_eq!(tool["delta"]["partial_json"], "{\"a\"");

        let end = api_event_json(&StreamEventKind::MessageDelta {
            stop_reason: Some(StopReason::MaxTokens),
            usage: Usage {
                output_tokens: 5,
                ..Usage::default()
            },
        });
        assert_eq!(end["delta"]["stop_reason"], "max_tokens");
        assert_eq!(end["usage"]["output_tokens"], 5);
    }
}
