//! Anthropic wire → IR: stream events, usage, stop reason and errors.
//!
//! Pure functions only; no I/O. The SDK types stay on this side of the adapter
//! boundary.

use std::collections::BTreeMap;
use std::time::Duration;

use anthropic_sdk::ApiError;
use anthropic_sdk::resources::messages as sdk;
use serde_json::Value;

use crate::services::provider::ir::{
    BlockStart, Delta, Protocol, ProviderError, ProviderErrorKind, ProviderState, StopReason,
    StreamEvent, StreamEventKind, Usage,
};

fn non_negative(value: i64) -> u64 {
    value.max(0) as u64
}

/// The undocumented `cache_deleted_input_tokens` usage extension (cached
/// microcompact responses). The typed SDK usage structs drop unknown fields, so
/// it can only be read from the raw wire JSON. `usage` is that JSON object.
fn cache_deleted_tokens(usage: Option<&Value>) -> u64 {
    usage
        .and_then(|usage| usage.get("cache_deleted_input_tokens")?.as_i64())
        .map(non_negative)
        .unwrap_or(0)
}

/// `cache_creation.ephemeral_1h_input_tokens`, read from the raw wire usage
/// for the same reason as `cache_deleted_tokens`.
fn cache_write_1h_tokens(usage: Option<&Value>) -> u64 {
    usage
        .and_then(|usage| {
            usage
                .pointer("/cache_creation/ephemeral_1h_input_tokens")?
                .as_i64()
        })
        .map(non_negative)
        .unwrap_or(0)
}

pub fn usage_from_message(usage: &sdk::Usage, raw_usage: Option<&Value>) -> Usage {
    Usage {
        input_tokens: non_negative(usage.input_tokens),
        output_tokens: non_negative(usage.output_tokens),
        cache_write_tokens: usage
            .cache_creation_input_tokens
            .map(non_negative)
            .unwrap_or(0),
        cache_write_1h_tokens: cache_write_1h_tokens(raw_usage),
        cache_read_tokens: usage.cache_read_input_tokens.map(non_negative).unwrap_or(0),
        cache_deleted_tokens: cache_deleted_tokens(raw_usage),
        reasoning_tokens: 0,
    }
}

/// `message_delta` carries cumulative output tokens, and input-side fields only
/// when the server restates them. A missing field keeps the earlier value; an
/// explicit zero input-side count also keeps it (the server sends 0 there).
pub fn merge_delta_usage(
    current: Usage,
    delta: &sdk::MessageDeltaUsage,
    raw_usage: Option<&Value>,
) -> Usage {
    let keep_unless_positive = |new: Option<i64>, old: u64| match new.map(non_negative) {
        Some(value) if value > 0 => value,
        _ => old,
    };
    let deleted = cache_deleted_tokens(raw_usage);
    Usage {
        input_tokens: keep_unless_positive(delta.input_tokens, current.input_tokens),
        output_tokens: non_negative(delta.output_tokens),
        cache_write_tokens: keep_unless_positive(
            delta.cache_creation_input_tokens,
            current.cache_write_tokens,
        ),
        cache_write_1h_tokens: match cache_write_1h_tokens(raw_usage) {
            0 => current.cache_write_1h_tokens,
            value => value,
        },
        cache_read_tokens: keep_unless_positive(
            delta.cache_read_input_tokens,
            current.cache_read_tokens,
        ),
        cache_deleted_tokens: if deleted > 0 {
            deleted
        } else {
            current.cache_deleted_tokens
        },
        reasoning_tokens: current.reasoning_tokens,
    }
}

pub fn stop_reason(reason: sdk::StopReason) -> StopReason {
    match reason {
        sdk::StopReason::EndTurn => StopReason::EndTurn,
        sdk::StopReason::ToolUse => StopReason::ToolUse,
        sdk::StopReason::MaxTokens => StopReason::MaxTokens,
        sdk::StopReason::StopSequence => StopReason::StopSequence,
        sdk::StopReason::PauseTurn => StopReason::PauseTurn,
        sdk::StopReason::Refusal => StopReason::Refusal,
    }
}

fn initial_tool_input(input: Value) -> Value {
    // A streamed tool call starts with `{}`; the real arguments arrive as
    // `input_json_delta` fragments.
    input
}

fn block_start(block: sdk::ContentBlock) -> BlockStart {
    match block {
        sdk::ContentBlock::Text { .. } => BlockStart::Text,
        sdk::ContentBlock::Thinking { .. } => BlockStart::Thinking,
        sdk::ContentBlock::RedactedThinking { data } => BlockStart::RedactedThinking { data },
        sdk::ContentBlock::ToolUse { id, input, name } => BlockStart::ToolCall {
            id,
            name,
            server: false,
            input: initial_tool_input(input),
        },
        sdk::ContentBlock::ServerToolUse { id, input, name } => BlockStart::ToolCall {
            id,
            name,
            server: true,
            input: initial_tool_input(input),
        },
        sdk::ContentBlock::WebSearchToolResult {
            content,
            tool_use_id,
        } => BlockStart::ServerToolResult {
            tool_call_id: tool_use_id,
            name: "web_search".to_string(),
            content: serde_json::to_value(content).unwrap_or(Value::Null),
        },
    }
}

fn delta(delta: sdk::ContentBlockDelta) -> Option<Delta> {
    match delta {
        sdk::ContentBlockDelta::TextDelta { text } => Some(Delta::Text { text }),
        sdk::ContentBlockDelta::ThinkingDelta { thinking } => {
            Some(Delta::Thinking { text: thinking })
        }
        sdk::ContentBlockDelta::SignatureDelta { signature } => {
            Some(Delta::Signature { signature })
        }
        sdk::ContentBlockDelta::InputJsonDelta { partial_json } => {
            Some(Delta::ToolInput { json: partial_json })
        }
        // Citations are not part of the neutral IR yet.
        sdk::ContentBlockDelta::CitationsDelta { .. } => None,
    }
}

/// Running state across one message's events.
#[derive(Debug, Default)]
pub struct StreamNormalizer {
    usage: Usage,
}

impl StreamNormalizer {
    /// Translate one wire event. `None` means the event has no IR counterpart
    /// (ping, citations).
    pub fn event(
        &mut self,
        event: sdk::MessageStreamEvent,
        wire: Option<Value>,
    ) -> Option<StreamEvent> {
        let kind = match event {
            sdk::MessageStreamEvent::MessageStart { message } => {
                let raw_usage = wire
                    .as_ref()
                    .and_then(|wire| wire.pointer("/message/usage"));
                self.usage = usage_from_message(&message.usage, raw_usage);
                StreamEventKind::MessageStart {
                    request_id: message.request_id,
                    message_id: Some(message.id),
                    model: Some(message.model),
                    usage: self.usage,
                }
            }
            sdk::MessageStreamEvent::ContentBlockStart {
                content_block,
                index,
            } => StreamEventKind::BlockStart {
                index,
                block: block_start(content_block),
            },
            sdk::MessageStreamEvent::ContentBlockDelta { delta: d, index } => {
                StreamEventKind::BlockDelta {
                    index,
                    delta: delta(d)?,
                }
            }
            sdk::MessageStreamEvent::ContentBlockStop { index } => {
                StreamEventKind::BlockStop { index }
            }
            sdk::MessageStreamEvent::MessageDelta {
                delta: d,
                usage: delta_usage,
            } => {
                let raw_usage = wire.as_ref().and_then(|wire| wire.get("usage"));
                self.usage = merge_delta_usage(self.usage, &delta_usage, raw_usage);
                StreamEventKind::MessageDelta {
                    stop_reason: d.stop_reason.map(stop_reason),
                    usage: self.usage,
                }
            }
            sdk::MessageStreamEvent::MessageStop => StreamEventKind::MessageStop,
            _ => return None,
        };
        Some(StreamEvent { kind, wire })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn parse_retry_after(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let value = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))?
        .1
        .trim();
    value
        .parse::<f64>()
        .ok()
        .filter(|s| *s >= 0.0)
        .map(Duration::from_secs_f64)
}

/// A 400 that says the prompt plus `max_tokens` exceeds the window.
fn is_context_overflow(status: Option<u16>, message: &str) -> bool {
    status == Some(400)
        && (message.contains("input length and `max_tokens` exceed context limit")
            || message.contains("prompt is too long")
            || message.contains("exceed context limit"))
}

pub fn provider_error(error: &ApiError) -> ProviderError {
    let status = error.status();
    let message = error.to_string();
    let headers: BTreeMap<String, String> = error
        .headers()
        .map(|headers| {
            headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let kind = match error {
        ApiError::UserAbort { .. } => ProviderErrorKind::Aborted,
        ApiError::Connection { .. } | ApiError::ConnectionTimeout { .. } => {
            ProviderErrorKind::Transport
        }
        ApiError::Sdk(_) => ProviderErrorKind::InvalidRequest,
        _ => match status {
            Some(429) => ProviderErrorKind::RateLimited,
            Some(529) => ProviderErrorKind::Overloaded,
            Some(401) => ProviderErrorKind::Unauthorized,
            Some(403) => ProviderErrorKind::Forbidden,
            Some(404) => ProviderErrorKind::NotFound,
            Some(400) if is_context_overflow(status, &message) => {
                ProviderErrorKind::ContextOverflow
            }
            Some(400) | Some(409) | Some(422) => ProviderErrorKind::InvalidRequest,
            Some(408) => ProviderErrorKind::Transient { retryable: true },
            Some(s) if s >= 500 => ProviderErrorKind::Transient { retryable: true },
            _ => ProviderErrorKind::Transient { retryable: false },
        },
    };
    ProviderError {
        kind,
        message,
        status,
        retry_after: parse_retry_after(&headers),
        request_id: error.request_id().map(str::to_string),
        headers,
        body: error.body().cloned(),
    }
}

/// Replay state this adapter attaches to thinking/tool parts.
pub fn state(data: Value) -> ProviderState {
    ProviderState {
        protocol: Protocol::AnthropicMessages,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(raw: &[Value]) -> Vec<StreamEventKind> {
        let mut normalizer = StreamNormalizer::default();
        raw.iter()
            .filter_map(|value| {
                let event: sdk::MessageStreamEvent = serde_json::from_value(value.clone()).ok()?;
                normalizer.event(event, Some(value.clone())).map(|e| e.kind)
            })
            .collect()
    }

    fn message_start(input: i64) -> Value {
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x",
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": input, "output_tokens": 1,
                          "cache_read_input_tokens": 40, "cache_creation_input_tokens": 7}
            }
        })
    }

    #[test]
    fn one_hour_cache_writes_are_split_out_of_the_cache_write_count() {
        let mut start = message_start(10);
        start["message"]["usage"]["cache_creation"] =
            serde_json::json!({"ephemeral_5m_input_tokens": 3, "ephemeral_1h_input_tokens": 4});
        let kinds = events(&[
            start,
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                               "usage":{"output_tokens":9}}),
        ]);
        let usages = kinds
            .iter()
            .filter_map(|kind| match kind {
                StreamEventKind::MessageStart { usage, .. }
                | StreamEventKind::MessageDelta { usage, .. } => Some(*usage),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(usages.len(), 2);
        for usage in usages {
            assert_eq!(
                (usage.cache_write_tokens, usage.cache_write_1h_tokens),
                (7, 4)
            );
        }
    }

    #[test]
    fn text_block_streams_through_as_start_delta_stop() {
        let kinds = events(&[
            message_start(10),
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
        ]);
        assert!(matches!(
            &kinds[0],
            StreamEventKind::MessageStart { message_id: Some(id), usage, .. }
                if id == "msg_1" && usage.cache_read_tokens == 40 && usage.cache_write_tokens == 7
        ));
        assert_eq!(
            kinds[1],
            StreamEventKind::BlockStart {
                index: 0,
                block: BlockStart::Text
            }
        );
        assert_eq!(
            kinds[3],
            StreamEventKind::BlockDelta {
                index: 0,
                delta: Delta::Text { text: "lo".into() }
            }
        );
        assert_eq!(kinds[4], StreamEventKind::BlockStop { index: 0 });
    }

    #[test]
    fn thinking_block_keeps_signature_delta() {
        let kinds = events(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}}),
        ]);
        assert_eq!(
            kinds[0],
            StreamEventKind::BlockStart {
                index: 0,
                block: BlockStart::Thinking
            }
        );
        assert_eq!(
            kinds[2],
            StreamEventKind::BlockDelta {
                index: 0,
                delta: Delta::Signature {
                    signature: "sig==".into()
                }
            }
        );
    }

    #[test]
    fn tool_use_arguments_arrive_as_json_fragments() {
        let kinds = events(&[
            serde_json::json!({"type":"content_block_start","index":1,"content_block":
                {"type":"tool_use","id":"toolu_1","name":"Read","input":{}}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file"}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\":\"a\"}"}}),
        ]);
        assert_eq!(
            kinds[0],
            StreamEventKind::BlockStart {
                index: 1,
                block: BlockStart::ToolCall {
                    id: "toolu_1".into(),
                    name: "Read".into(),
                    server: false,
                    input: serde_json::json!({}),
                }
            }
        );
        let joined: String = kinds[1..]
            .iter()
            .filter_map(|kind| match kind {
                StreamEventKind::BlockDelta {
                    delta: Delta::ToolInput { json },
                    ..
                } => Some(json.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(joined, "{\"file\":\"a\"}");
    }

    #[test]
    fn server_tool_use_is_flagged_server() {
        let kinds = events(&[
            serde_json::json!({"type":"content_block_start","index":0,"content_block":
            {"type":"server_tool_use","id":"srv_1","name":"web_search","input":{}}}),
        ]);
        assert!(matches!(
            &kinds[0],
            StreamEventKind::BlockStart {
                block: BlockStart::ToolCall { server: true, .. },
                ..
            }
        ));
    }

    #[test]
    fn message_delta_merges_usage_and_maps_stop_reason() {
        let kinds = events(&[
            message_start(10),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
                "usage":{"output_tokens":55,"input_tokens":0,"cache_read_input_tokens":0}}),
        ]);
        match &kinds[1] {
            StreamEventKind::MessageDelta { stop_reason, usage } => {
                assert_eq!(*stop_reason, Some(StopReason::ToolUse));
                // Output is cumulative; explicit zeros do not erase message_start.
                assert_eq!(usage.output_tokens, 55);
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.cache_read_tokens, 40);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cache_deleted_tokens_survive_in_delta_usage() {
        let kinds = events(&[
            message_start(10),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                "usage":{"output_tokens":3,"cache_deleted_input_tokens":123}}),
        ]);
        match &kinds[1] {
            StreamEventKind::MessageDelta { usage, .. } => {
                // Read from the raw wire usage; the typed SDK struct drops it.
                assert_eq!(usage.cache_deleted_tokens, 123);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cache_deleted_tokens_are_read_from_message_start_usage_too() {
        let mut start = message_start(10);
        start["message"]["usage"]["cache_deleted_input_tokens"] = serde_json::json!(9);
        match &events(&[start])[0] {
            StreamEventKind::MessageStart { usage, .. } => {
                assert_eq!(usage.cache_deleted_tokens, 9)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ping_has_no_ir_event() {
        assert!(events(&[serde_json::json!({"type":"ping"})]).is_empty());
    }

    fn api_error(status: u16, message: &str, headers: &[(&str, &str)]) -> ApiError {
        ApiError::generate(
            Some(status),
            Some(serde_json::json!({"error": {"type": "x", "message": message}})),
            Some(message.to_string()),
            Some(
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
        )
    }

    #[test]
    fn errors_are_classified_by_status() {
        let cases = [
            (429, ProviderErrorKind::RateLimited),
            (529, ProviderErrorKind::Overloaded),
            (401, ProviderErrorKind::Unauthorized),
            (403, ProviderErrorKind::Forbidden),
            (404, ProviderErrorKind::NotFound),
            (400, ProviderErrorKind::InvalidRequest),
            (500, ProviderErrorKind::Transient { retryable: true }),
            (503, ProviderErrorKind::Transient { retryable: true }),
        ];
        for (status, expected) in cases {
            let error = provider_error(&api_error(status, "boom", &[]));
            assert_eq!(error.kind, expected, "status {status}");
            assert_eq!(error.status, Some(status));
        }
    }

    #[test]
    fn rate_limit_reads_retry_after_and_request_id() {
        let error = provider_error(&api_error(
            429,
            "slow",
            &[("retry-after", "7"), ("request-id", "req_9")],
        ));
        assert_eq!(error.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(error.request_id.as_deref(), Some("req_9"));
    }

    #[test]
    fn context_overflow_is_a_400_with_the_window_message() {
        let error = provider_error(&api_error(
            400,
            "input length and `max_tokens` exceed context limit: 1 + 2 > 3",
            &[],
        ));
        assert_eq!(error.kind, ProviderErrorKind::ContextOverflow);
    }

    #[test]
    fn transport_and_abort_have_no_status() {
        let timeout = provider_error(&ApiError::ConnectionTimeout {
            message: "t".into(),
        });
        assert_eq!(timeout.kind, ProviderErrorKind::Transport);
        let abort = provider_error(&ApiError::UserAbort {
            message: "a".into(),
        });
        assert_eq!(abort.kind, ProviderErrorKind::Aborted);
        assert_eq!(abort.status, None);
    }
}
