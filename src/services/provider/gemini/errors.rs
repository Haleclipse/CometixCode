//! Google Gemini error body and status -> normalized [`ProviderError`].
//!
//! @nyan: docs/nyan/provider-redesign.md §7.1, §7.3. Maps Google RPC error
//! statuses (e.g. `RESOURCE_EXHAUSTED`, `UNAVAILABLE`, `PERMISSION_DENIED`)
//! and HTTP status codes to normalized IR [`ProviderError`] kinds.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use crate::services::provider::ir::{ProviderError, ProviderErrorKind};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Parsed Gemini/Google error details from JSON response.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GeminiErrorBody {
    pub message: Option<String>,
    pub status: Option<String>,
    pub code: Option<u16>,
    pub retry_delay: Option<Duration>,
}

fn parse_duration_string(text: &str) -> Option<Duration> {
    let trimmed = text.trim();
    let num_str = trimmed.strip_suffix('s')?;
    let secs = num_str.parse::<f64>().ok()?;
    if secs >= 0.0 {
        Some(Duration::from_secs_f64(secs))
    } else {
        None
    }
}

pub fn parse_error_body(body: &Value) -> GeminiErrorBody {
    let err_obj = body.get("error").filter(|v| v.is_object()).unwrap_or(body);
    let message = err_obj
        .get("message")
        .and_then(Value::as_str)
        .map(String::from);
    let status = err_obj
        .get("status")
        .and_then(Value::as_str)
        .map(String::from);
    let code = err_obj
        .get("code")
        .and_then(Value::as_u64)
        .map(|c| c as u16);

    let mut retry_delay = None;
    if let Some(details) = err_obj.get("details").and_then(Value::as_array) {
        for detail in details {
            if detail.get("@type").and_then(Value::as_str)
                == Some("type.googleapis.com/google.rpc.RetryInfo")
            {
                if let Some(delay_str) = detail.get("retryDelay").and_then(Value::as_str) {
                    retry_delay = parse_duration_string(delay_str);
                }
            }
        }
    }

    GeminiErrorBody {
        message,
        status,
        code,
        retry_delay,
    }
}

fn mentions_context_overflow(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "maximum context length",
        "context window",
        "context_length_exceeded",
        "too many tokens",
        "token count exceeds",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn kind_from_google_status(status_str: &str) -> Option<ProviderErrorKind> {
    match status_str {
        "RESOURCE_EXHAUSTED" => Some(ProviderErrorKind::RateLimited),
        "UNAVAILABLE" => Some(ProviderErrorKind::Overloaded),
        "UNAUTHENTICATED" => Some(ProviderErrorKind::Unauthorized),
        "PERMISSION_DENIED" => Some(ProviderErrorKind::Forbidden),
        "NOT_FOUND" => Some(ProviderErrorKind::NotFound),
        "INVALID_ARGUMENT" => Some(ProviderErrorKind::InvalidRequest),
        "INTERNAL" | "DEADLINE_EXCEEDED" => Some(ProviderErrorKind::Transient { retryable: true }),
        _ => None,
    }
}

fn kind_from_http_status(status: u16) -> ProviderErrorKind {
    match status {
        401 => ProviderErrorKind::Unauthorized,
        403 => ProviderErrorKind::Forbidden,
        404 => ProviderErrorKind::NotFound,
        413 => ProviderErrorKind::ContextOverflow,
        429 => ProviderErrorKind::RateLimited,
        503 => ProviderErrorKind::Overloaded,
        400 | 409 | 422 => ProviderErrorKind::InvalidRequest,
        408 => ProviderErrorKind::Transient { retryable: true },
        status if status >= 500 => ProviderErrorKind::Transient { retryable: true },
        _ => ProviderErrorKind::Transient { retryable: false },
    }
}

pub fn retry_after_header(headers: &BTreeMap<String, String>) -> Option<Duration> {
    crate::services::provider::openai::errors::retry_after(headers)
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

/// Classify a non-2xx HTTP response from Google Gemini API into [`ProviderError`].
pub fn from_http(
    status: u16,
    headers: BTreeMap<String, String>,
    body_text: &str,
    request_id: Option<String>,
) -> ProviderError {
    let truncated = truncate_on_char_boundary(body_text, MAX_BODY_BYTES);
    let body: Option<Value> = serde_json::from_str(truncated).ok();
    let parsed = body.as_ref().map(parse_error_body).unwrap_or_default();

    let mut kind = parsed
        .status
        .as_deref()
        .and_then(kind_from_google_status)
        .unwrap_or_else(|| kind_from_http_status(status));

    // HTTP status is authoritative for 401 / 403 / 404
    if matches!(status, 401 | 403 | 404) {
        kind = kind_from_http_status(status);
    }

    if kind == ProviderErrorKind::InvalidRequest
        && parsed
            .message
            .as_deref()
            .is_some_and(mentions_context_overflow)
    {
        kind = ProviderErrorKind::ContextOverflow;
    }

    let retry_after = parsed.retry_delay.or_else(|| retry_after_header(&headers));

    let message = parsed.message.clone().unwrap_or_else(|| {
        if body_text.trim().is_empty() {
            format!("HTTP status {status}")
        } else {
            truncated.to_string()
        }
    });

    ProviderError {
        kind,
        message,
        status: Some(status),
        retry_after,
        request_id,
        headers,
        body,
    }
}

/// Classify an error returned inside a stream frame.
pub fn from_stream_error(error_value: &Value) -> ProviderError {
    let parsed = parse_error_body(error_value);
    let status = parsed.code.unwrap_or(500);

    let mut kind = parsed
        .status
        .as_deref()
        .and_then(kind_from_google_status)
        .unwrap_or_else(|| kind_from_http_status(status));

    if kind == ProviderErrorKind::InvalidRequest
        && parsed
            .message
            .as_deref()
            .is_some_and(mentions_context_overflow)
    {
        kind = ProviderErrorKind::ContextOverflow;
    }

    let message = parsed
        .message
        .unwrap_or_else(|| "Gemini stream error".to_string());

    ProviderError {
        kind,
        message,
        status: Some(status),
        retry_after: parsed.retry_delay,
        request_id: None,
        headers: BTreeMap::new(),
        body: Some(error_value.clone()),
    }
}
