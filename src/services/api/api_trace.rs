//! API request lifecycle tracing.
//!
//! Writes stage events to `~/.claude/trace/{session}.jsonl` (or
//! `COMETIX_TRACE_DIR`) and mirrors summaries into `--debug` logs as
//! `api: [API:trace] ...` so `--debug=api` filters match.
//!
//! DEVIATION(SCOPE): local development instrumentation with no CC counterpart.
//! CC keeps no per-request trace store and writes no stage file; its nearest
//! surface is the `--debug` log and its category filter
//! (`utils/debug.ts:203-228`, `utils/debugFilter.ts:9-14`), which the `api:`
//! prefix here feeds so official filtering still applies. Nothing in this
//! module reaches the model or the transcript.
//!
//! Env:
//! - `COMETIX_TRACE=off|0|1|summary|true|full`
//! - `COMETIX_TRACE_DIR=<dir>` overrides the default trace directory
//! - `--debug=api` (or unfiltered `--debug`) implies at least summary

use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

thread_local! {
    static CURRENT_TRACE: RefCell<Option<ApiTrace>> = const { RefCell::new(None) };
}

/// Bind a trace for the current request/attempt (thread-local).
pub fn set_current_trace(trace: Option<ApiTrace>) {
    CURRENT_TRACE.with(|slot| {
        *slot.borrow_mut() = trace;
    });
}

/// Read the current request trace, if any.
pub fn current_trace() -> Option<ApiTrace> {
    CURRENT_TRACE.with(|slot| slot.borrow().clone())
}

/// Emit on the current trace if one is bound.
pub fn emit_current(stage: &str, data: Value) {
    if let Some(trace) = current_trace() {
        emit(&trace, stage, data);
    }
}

/// Trace verbosity resolved from env + debug mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceLevel {
    Off,
    Summary,
    Full,
}

impl TraceLevel {
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Per-request handle carrying a stable `trace_id` across stages.
#[derive(Debug, Clone)]
pub struct ApiTrace {
    pub trace_id: String,
    pub attempt: u32,
}

impl ApiTrace {
    pub fn new(attempt: u32) -> Self {
        Self {
            trace_id: uuid::Uuid::new_v4().to_string(),
            attempt,
        }
    }

    pub fn with_attempt(&self, attempt: u32) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            attempt,
        }
    }
}

/// Resolve effective trace level.
pub fn trace_level() -> TraceLevel {
    match crate::utils::process_env::var("COMETIX_TRACE") {
        Some(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "" | "0" | "off" | "false" | "no" => {
                    // Explicit off still allows --debug=api to re-enable summary.
                    if debug_implies_summary() {
                        TraceLevel::Summary
                    } else {
                        TraceLevel::Off
                    }
                }
                "full" => TraceLevel::Full,
                "1" | "summary" | "true" | "yes" | "on" => TraceLevel::Summary,
                _ => TraceLevel::Summary,
            }
        }
        None => {
            if debug_implies_summary() {
                TraceLevel::Summary
            } else {
                TraceLevel::Off
            }
        }
    }
}

fn debug_implies_summary() -> bool {
    if !crate::utils::debug::is_debug_mode() {
        return false;
    }
    crate::utils::debug::debug_filter_matches_api()
}

/// Directory for trace JSONL files.
pub fn get_trace_dir() -> PathBuf {
    if let Some(dir) = crate::utils::process_env::var("COMETIX_TRACE_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::utils::env_utils::get_claude_config_home_dir().join("trace")
}

/// Path for the current session's trace file.
pub fn get_trace_path() -> PathBuf {
    get_trace_dir().join(format!(
        "{}.jsonl",
        crate::bootstrap::state::get_session_id()
    ))
}

/// Emit a stage event. No-op when tracing is off.
pub fn emit(trace: &ApiTrace, stage: &str, data: Value) {
    let force_detail = stage.contains("error") || stage.contains("failed");
    emit_with_level(trace, stage, data, force_detail);
}

/// Emit diagnostic stages with full (redacted) detail even in summary mode.
pub fn emit_always_when_enabled(trace: &ApiTrace, stage: &str, data: Value) {
    emit_with_level(trace, stage, data, true);
}

fn emit_with_level(trace: &ApiTrace, stage: &str, mut data: Value, force_full_payload: bool) {
    let level = trace_level();
    if !level.is_enabled() {
        return;
    }

    if !level.is_full() && !force_full_payload {
        data = summarize_payload(data);
    } else {
        data = redact_value(data);
    }

    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let entry = json!({
        "timestamp": timestamp,
        "trace_id": trace.trace_id,
        "attempt": trace.attempt,
        "stage": stage,
        "data": data,
    });

    let summary = format!(
        "api: [API:trace] stage={stage} trace_id={} attempt={}",
        &trace.trace_id[..trace.trace_id.len().min(8)],
        trace.attempt
    );
    let detail = match level {
        TraceLevel::Full => format!("{summary} {}", compact_json(&entry)),
        _ => {
            if stage.contains("error") || stage.contains("failed") {
                format!("{summary} {}", compact_json(&data))
            } else {
                summary
            }
        }
    };

    let log_level = if stage.contains("error") || stage.contains("failed") {
        crate::utils::debug::DebugLogLevel::Error
    } else {
        crate::utils::debug::DebugLogLevel::Debug
    };
    crate::utils::debug::log_for_debugging_with_level(&detail, log_level);

    append_trace_line(&entry);
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn append_trace_line(entry: &Value) {
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let path = get_trace_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Serialize appends across threads.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn summarize_payload(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                match key.as_str() {
                    "messages" | "local_messages" | "sdk_messages" | "body" | "params"
                    | "content" | "blocks" | "snippet" | "failed_block" => {
                        out.insert(key, summarize_heavy(child));
                    }
                    _ => {
                        out.insert(key, summarize_payload(child));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(8)
                .map(summarize_payload)
                .collect::<Vec<_>>(),
        ),
        other => redact_value(other),
    }
}

fn summarize_heavy(value: Value) -> Value {
    match value {
        Value::Array(items) => json!({
            "len": items.len(),
            "roles": items.iter().filter_map(|item| {
                item.get("role").and_then(|r| r.as_str()).map(|s| s.to_string())
            }).collect::<Vec<_>>(),
            "block_types": items.iter().flat_map(|item| {
                item.get("content").and_then(|c| c.as_array()).into_iter().flatten().filter_map(|block| {
                    block.get("type").and_then(|t| t.as_str()).map(|s| s.to_string())
                })
            }).take(32).collect::<Vec<_>>(),
        }),
        Value::String(s) => json!({
            "chars": s.len(),
            "preview": truncate_str(&s, 240),
        }),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            out.insert(
                "keys".to_string(),
                Value::Array(map.keys().cloned().map(Value::String).collect()),
            );
            if let Some(role) = map.get("role") {
                out.insert("role".to_string(), role.clone());
            }
            if let Some(ty) = map.get("type") {
                out.insert("type".to_string(), ty.clone());
            }
            Value::Object(out)
        }
        other => other,
    }
}

/// Redact secrets and shrink large base64 blobs for full-mode payloads.
pub fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                let key_lower = key.to_ascii_lowercase();
                if matches!(
                    key_lower.as_str(),
                    "authorization"
                        | "api_key"
                        | "apikey"
                        | "x-api-key"
                        | "anthropic_api_key"
                        | "oauth_token"
                        | "access_token"
                        | "refresh_token"
                        | "password"
                        | "secret"
                ) {
                    out.insert(key, Value::String("<redacted>".to_string()));
                } else if key_lower == "data"
                    && child
                        .as_str()
                        .is_some_and(|s| s.len() > 256 && looks_like_base64(s))
                {
                    let s = child.as_str().unwrap_or_default();
                    out.insert(
                        key,
                        json!({
                            "bytes_len": s.len(),
                            "preview": "<base64 redacted>",
                        }),
                    );
                } else {
                    out.insert(key, redact_value(child));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::String(s) => {
            if s.len() > 64
                && (s.contains("sk-ant-")
                    || s.contains("ANTHROPIC_API_KEY")
                    || s.contains("Bearer "))
            {
                Value::String("<redacted>".to_string())
            } else {
                Value::String(s)
            }
        }
        other => other,
    }
}

fn looks_like_base64(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// Truncate a JSON value for error snippets (≤2KB).
pub fn truncate_json_snippet(value: &Value, max_bytes: usize) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    if rendered.len() <= max_bytes {
        return rendered;
    }
    let keep = max_bytes.saturating_sub("…".len()).max(1);
    // Avoid splitting a multibyte UTF-8 boundary.
    let mut end = keep.min(rendered.len());
    while end > 0 && !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &rendered[..end])
}

/// Inspect content blocks on a local MessageParam for diagnostics.
pub fn content_block_types(param: &super::claude::MessageParam) -> Vec<String> {
    match &param.content {
        super::claude::MessageContent::Text(_) => vec!["text(string)".to_string()],
        super::claude::MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown")
                    .to_string()
            })
            .collect(),
    }
}

/// Try converting a single content block to SDK ContentBlockParam for diagnosis.
pub fn diagnose_block_convert_error(block: &Value) -> Option<String> {
    let result = serde_json::from_value::<anthropic_sdk::resources::messages::ContentBlockParam>(
        block.clone(),
    );
    match result {
        Ok(_) => None,
        Err(error) => {
            let block_type = block
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            Some(format!("type={block_type}: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_authorization_and_large_base64() {
        let value = json!({
            "Authorization": "Bearer secret",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "A".repeat(300),
            }
        });
        let redacted = redact_value(value);
        assert_eq!(redacted["Authorization"], "<redacted>");
        assert_eq!(redacted["source"]["data"]["preview"], "<base64 redacted>");
        assert_eq!(redacted["source"]["data"]["bytes_len"], 300);
    }

    #[test]
    fn truncate_json_snippet_caps_bytes() {
        let value = json!({ "text": "x".repeat(100) });
        let snippet = truncate_json_snippet(&value, 40);
        assert!(snippet.len() <= 41);
        assert!(snippet.ends_with('…'));
    }
}
