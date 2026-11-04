//! OpenAI error body and status -> normalized [`ProviderError`].
//!
//! @nyan: docs/nyan/provider-redesign.md §7.1. Pi classifies by regex on the
//! rendered message; the structured `error.code` / `error.type` OpenAI sends is
//! more reliable, so it is read first and the message is only a fallback for
//! context overflow, where gateways rewrite the code. The body is kept on the
//! error for retry policy and is never logged or shown verbatim; the message
//! shown to the user is OpenAI's own `error.message`, which carries no
//! credential.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use crate::services::provider::ir::{ProviderError, ProviderErrorKind};

/// The longest body kept for classification and display.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// `{"error": {"message", "type", "param", "code"}}`, or the flat form some
/// gateways use. Missing or non-string fields are `None`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ErrorBody {
    pub message: Option<String>,
    pub kind: Option<String>,
    pub code: Option<String>,
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        // `code` is a number on some gateways.
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub fn parse_error_body(body: &Value) -> ErrorBody {
    let inner = body.get("error").filter(|value| value.is_object());
    let source = inner.unwrap_or(body);
    ErrorBody {
        message: string_at(source, "message"),
        kind: string_at(source, "type"),
        code: string_at(source, "code"),
    }
}

fn mentions_context_overflow(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "maximum context length",
        "context window",
        "context_length_exceeded",
        "too many tokens",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Map an OpenAI error `code`/`type` (HTTP or in-stream) to a kind. `None`
/// means the code says nothing and the caller falls back to the status.
fn kind_from_code(body: &ErrorBody) -> Option<ProviderErrorKind> {
    let code = body.code.as_deref().map(str::to_ascii_lowercase);
    let kind = body.kind.as_deref().map(str::to_ascii_lowercase);
    let named = |needle: &str| code.as_deref() == Some(needle) || kind.as_deref() == Some(needle);
    if named("context_length_exceeded") {
        return Some(ProviderErrorKind::ContextOverflow);
    }
    if named("rate_limit_exceeded") || named("insufficient_quota") || named("rate_limit_error") {
        return Some(ProviderErrorKind::RateLimited);
    }
    if named("invalid_api_key") || named("authentication_error") {
        return Some(ProviderErrorKind::Unauthorized);
    }
    if named("server_error") || named("internal_error") || named("service_unavailable") {
        return Some(ProviderErrorKind::Transient { retryable: true });
    }
    if named("invalid_request_error")
        || named("invalid_prompt")
        || named("invalid_value")
        || named("unsupported_value")
    {
        return Some(ProviderErrorKind::InvalidRequest);
    }
    None
}

fn kind_from_status(status: u16) -> ProviderErrorKind {
    match status {
        401 => ProviderErrorKind::Unauthorized,
        403 => ProviderErrorKind::Forbidden,
        404 => ProviderErrorKind::NotFound,
        413 => ProviderErrorKind::ContextOverflow,
        429 => ProviderErrorKind::RateLimited,
        400 | 409 | 422 => ProviderErrorKind::InvalidRequest,
        408 => ProviderErrorKind::Transient { retryable: true },
        status if status >= 500 => ProviderErrorKind::Transient { retryable: true },
        _ => ProviderErrorKind::Transient { retryable: false },
    }
}

/// `retry-after-ms` first (OpenAI sends it), then `retry-after` in seconds.
pub fn retry_after(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let find = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    if let Some(ms) = find("retry-after-ms").and_then(|v| v.parse::<f64>().ok())
        && ms >= 0.0
    {
        return Some(Duration::from_secs_f64(ms / 1000.0));
    }
    find("retry-after")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|seconds| *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A non-2xx HTTP response.
pub fn from_http(
    status: u16,
    headers: BTreeMap<String, String>,
    body_text: &str,
    request_id: Option<String>,
) -> ProviderError {
    let truncated = truncate_on_char_boundary(body_text, MAX_BODY_BYTES);
    let body: Option<Value> = serde_json::from_str(truncated).ok();
    let parsed = body.as_ref().map(parse_error_body).unwrap_or_default();

    let mut kind = kind_from_code(&parsed).unwrap_or_else(|| kind_from_status(status));
    // The status is authoritative for auth and routing failures; a code can
    // only refine a status that is itself ambiguous (400, 429, 5xx).
    if matches!(status, 401 | 403 | 404) {
        kind = kind_from_status(status);
    }
    if kind == ProviderErrorKind::InvalidRequest
        && parsed
            .message
            .as_deref()
            .is_some_and(mentions_context_overflow)
    {
        kind = ProviderErrorKind::ContextOverflow;
    }

    let message = parsed.message.clone().unwrap_or_else(|| {
        let snippet: String = truncated.chars().take(300).collect();
        if snippet.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            snippet
        }
    });
    ProviderError {
        kind,
        message,
        status: Some(status),
        retry_after: retry_after(&headers),
        request_id,
        headers,
        body,
    }
}

/// An error event inside a 200 stream (Responses `error` / `response.failed`,
/// or a Chat chunk carrying `{"error": ...}`).
pub fn from_stream(body: &Value, request_id: Option<String>) -> ProviderError {
    let parsed = parse_error_body(body);
    let mut kind =
        kind_from_code(&parsed).unwrap_or(ProviderErrorKind::Transient { retryable: false });
    if parsed
        .message
        .as_deref()
        .is_some_and(mentions_context_overflow)
    {
        kind = ProviderErrorKind::ContextOverflow;
    }
    ProviderError {
        kind,
        message: parsed
            .message
            .unwrap_or_else(|| "the stream reported an error".to_string()),
        status: None,
        retry_after: None,
        request_id,
        headers: BTreeMap::new(),
        body: Some(body.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn http(status: u16, body: Value, headers: &[(&str, &str)]) -> ProviderError {
        from_http(
            status,
            headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &body.to_string(),
            Some("req_1".into()),
        )
    }

    fn api(message: &str, kind: &str, code: Value) -> Value {
        json!({"error": {"message": message, "type": kind, "param": null, "code": code}})
    }

    #[test]
    fn status_classes_map_to_the_normalized_kinds() {
        let cases = [
            (429, ProviderErrorKind::RateLimited),
            (401, ProviderErrorKind::Unauthorized),
            (403, ProviderErrorKind::Forbidden),
            (404, ProviderErrorKind::NotFound),
            (400, ProviderErrorKind::InvalidRequest),
            (422, ProviderErrorKind::InvalidRequest),
            (500, ProviderErrorKind::Transient { retryable: true }),
            (502, ProviderErrorKind::Transient { retryable: true }),
            (503, ProviderErrorKind::Transient { retryable: true }),
            (408, ProviderErrorKind::Transient { retryable: true }),
            (418, ProviderErrorKind::Transient { retryable: false }),
        ];
        for (status, expected) in cases {
            let error = http(status, api("boom", "x", Value::Null), &[]);
            assert_eq!(error.kind, expected, "status {status}");
            assert_eq!(error.status, Some(status));
            assert_eq!(error.request_id.as_deref(), Some("req_1"));
            assert_eq!(error.message, "boom");
        }
    }

    #[test]
    fn rate_limit_reads_retry_after_in_either_unit_and_prefers_milliseconds() {
        let seconds = http(429, api("slow", "x", Value::Null), &[("Retry-After", "7")]);
        assert_eq!(seconds.retry_after, Some(Duration::from_secs(7)));
        let millis = http(
            429,
            api("slow", "x", Value::Null),
            &[("retry-after", "7"), ("retry-after-ms", "1500")],
        );
        assert_eq!(millis.retry_after, Some(Duration::from_millis(1500)));
        let none = http(
            429,
            api("slow", "x", Value::Null),
            &[("retry-after", "soon")],
        );
        assert_eq!(none.retry_after, None);
    }

    #[test]
    fn the_error_code_refines_an_ambiguous_status() {
        let overflow = http(
            400,
            api(
                "This model's maximum context length is 8192 tokens.",
                "invalid_request_error",
                json!("context_length_exceeded"),
            ),
            &[],
        );
        assert_eq!(overflow.kind, ProviderErrorKind::ContextOverflow);
        // A gateway that drops the code still says it in words.
        let worded = http(
            400,
            api(
                "Input exceeds the context window of this model",
                "invalid_request_error",
                Value::Null,
            ),
            &[],
        );
        assert_eq!(worded.kind, ProviderErrorKind::ContextOverflow);
        let quota = http(
            429,
            api(
                "You exceeded your quota",
                "insufficient_quota",
                json!("insufficient_quota"),
            ),
            &[],
        );
        assert_eq!(quota.kind, ProviderErrorKind::RateLimited);
        // The code stays inspectable for the PR 8 policy that tells a billing
        // 429 from a burst 429.
        assert_eq!(
            quota.body.as_ref().unwrap()["error"]["code"],
            "insufficient_quota"
        );
    }

    #[test]
    fn the_status_wins_over_a_code_for_auth_and_routing_failures() {
        let error = http(
            401,
            api(
                "Incorrect API key",
                "invalid_request_error",
                json!("invalid_api_key"),
            ),
            &[],
        );
        assert_eq!(error.kind, ProviderErrorKind::Unauthorized);
        let not_found = http(
            404,
            api(
                "no such model",
                "invalid_request_error",
                json!("model_not_found"),
            ),
            &[],
        );
        assert_eq!(not_found.kind, ProviderErrorKind::NotFound);
    }

    #[test]
    fn a_non_json_body_still_yields_a_bounded_message() {
        let error = from_http(502, BTreeMap::new(), "<html>Bad Gateway</html>", None);
        assert_eq!(error.kind, ProviderErrorKind::Transient { retryable: true });
        assert!(error.message.contains("Bad Gateway"));
        assert!(error.body.is_none());
        let empty = from_http(500, BTreeMap::new(), "", None);
        assert_eq!(empty.message, "HTTP 500");
    }

    #[test]
    fn an_oversized_multibyte_body_is_cut_on_a_character_boundary() {
        let body = "é".repeat(MAX_BODY_BYTES);
        let error = from_http(500, BTreeMap::new(), &body, None);
        assert!(error.message.chars().all(|c| c == 'é'));
        assert_eq!(truncate_on_char_boundary("aé", 2), "a");
    }

    #[test]
    fn a_numeric_code_from_a_gateway_is_read() {
        let body = json!({"error": {"message": "m", "code": 429}});
        assert_eq!(parse_error_body(&body).code.as_deref(), Some("429"));
    }

    #[test]
    fn stream_errors_use_the_code_and_carry_no_status() {
        let failed = from_stream(
            &api("try later", "server_error", json!("server_error")),
            None,
        );
        assert_eq!(
            failed.kind,
            ProviderErrorKind::Transient { retryable: true }
        );
        assert_eq!(failed.status, None);
        let limited = from_stream(
            &json!({"code": "rate_limit_exceeded", "message": "slow"}),
            None,
        );
        assert_eq!(limited.kind, ProviderErrorKind::RateLimited);
        let unknown = from_stream(&json!({"message": "odd"}), None);
        assert_eq!(
            unknown.kind,
            ProviderErrorKind::Transient { retryable: false }
        );
    }
}
