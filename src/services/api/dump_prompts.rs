//! Request/response dump utility.
//!
//! Maps to: CC `services/api/dumpPrompts.ts`, but enabled via
//! `COMETIX_DUMP_PROMPTS=1` instead of ant-only USER_TYPE gating.
//!
//! Writes to `~/.claude/dump-prompts/{session}.jsonl`.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_CACHED_REQUESTS: usize = 5;

#[derive(Debug, Clone)]
struct DumpState {
    initialized: bool,
    message_count_seen: usize,
    last_init_data_hash: String,
    last_init_fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct CachedApiRequest {
    pub timestamp: String,
    pub request: Value,
}

static DUMP_STATE: Mutex<Option<HashMap<String, DumpState>>> = Mutex::new(None);
static CACHED_REQUESTS: Mutex<Vec<CachedApiRequest>> = Mutex::new(Vec::new());

fn dump_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("COMETIX_DUMP_PROMPTS").as_deref())
}

pub fn get_dump_prompts_path(agent_id_or_session_id: Option<&str>) -> PathBuf {
    let session = agent_id_or_session_id
        .map(|s| s.to_string())
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    crate::utils::env_utils::get_claude_config_home_dir()
        .join("dump-prompts")
        .join(format!("{session}.jsonl"))
}

pub fn get_last_api_requests() -> Vec<CachedApiRequest> {
    CACHED_REQUESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

pub fn clear_api_request_cache() {
    CACHED_REQUESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

pub fn clear_dump_state(agent_id_or_session_id: &str) {
    if let Ok(mut guard) = DUMP_STATE.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(agent_id_or_session_id);
        }
    }
}

pub fn clear_all_dump_state() {
    if let Ok(mut guard) = DUMP_STATE.lock() {
        *guard = Some(HashMap::new());
    }
}

/// Whether the process-global `dumpState` map still holds an entry for this
/// agent-or-session id. CC's map is module-private too (`dumpPrompts.ts:27`),
/// so the observable is membership, which is what
/// `clearDumpState`'s callers care about.
#[cfg(test)]
pub(crate) fn has_dump_state_for_test(agent_id_or_session_id: &str) -> bool {
    DUMP_STATE
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .as_ref()
                .map(|map| map.contains_key(agent_id_or_session_id))
        })
        .unwrap_or(false)
}

pub fn add_api_request_to_cache(request_data: Value) {
    let mut cache = CACHED_REQUESTS.lock().unwrap_or_else(|p| p.into_inner());
    cache.push(CachedApiRequest {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        request: crate::services::api::api_trace::redact_value(request_data),
    });
    if cache.len() > MAX_CACHED_REQUESTS {
        let drain = cache.len() - MAX_CACHED_REQUESTS;
        cache.drain(0..drain);
    }
}

fn hash_string(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn init_fingerprint(req: &serde_json::Map<String, Value>) -> String {
    let tools = req.get("tools").and_then(|t| t.as_array());
    let tool_names = tools
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let sys_len = match req.get("system") {
        Some(Value::String(s)) => s.len(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| {
                b.get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.len())
                    .unwrap_or(0)
            })
            .sum(),
        _ => 0,
    };
    let model = req
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    format!("{model}|{tool_names}|{sys_len}")
}

fn append_entries(file_path: &PathBuf, entries: &[String]) {
    if entries.is_empty() {
        return;
    }
    if let Some(parent) = file_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
    {
        for entry in entries {
            let _ = writeln!(file, "{entry}");
        }
    }
}

fn with_state<R>(session_id: &str, f: impl FnOnce(&mut DumpState) -> R) -> R {
    let mut guard = DUMP_STATE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    let map = guard.as_mut().expect("dump state map");
    let state = map
        .entry(session_id.to_string())
        .or_insert_with(|| DumpState {
            initialized: false,
            message_count_seen: 0,
            last_init_data_hash: String::new(),
            last_init_fingerprint: String::new(),
        });
    f(state)
}

/// Dump a request body object (already parsed JSON). Best-effort; never panics.
pub fn dump_request_value(request: &Value, session_id: Option<&str>) {
    let session = session_id
        .map(|s| s.to_string())
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    if !request.is_object() {
        return;
    }

    add_api_request_to_cache(request.clone());

    if !dump_enabled() {
        return;
    }

    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let file_path = get_dump_prompts_path(Some(&session));
    let redacted = crate::services::api::api_trace::redact_value(request.clone());
    let Some(redacted_obj) = redacted.as_object() else {
        return;
    };

    let mut entries = Vec::new();
    with_state(&session, |state| {
        let fingerprint = init_fingerprint(redacted_obj);
        if !state.initialized || fingerprint != state.last_init_fingerprint {
            let mut init_data = redacted_obj.clone();
            init_data.remove("messages");
            let init_data_str = serde_json::to_string(&init_data).unwrap_or_else(|_| "{}".into());
            let init_data_hash = hash_string(&init_data_str);
            state.last_init_fingerprint = fingerprint;
            if !state.initialized {
                state.initialized = true;
                state.last_init_data_hash = init_data_hash;
                entries.push(format!(
                    r#"{{"type":"init","timestamp":"{ts}","data":{init_data_str}}}"#
                ));
            } else if init_data_hash != state.last_init_data_hash {
                state.last_init_data_hash = init_data_hash;
                entries.push(format!(
                    r#"{{"type":"system_update","timestamp":"{ts}","data":{init_data_str}}}"#
                ));
            }
        }

        let messages = redacted_obj
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        for msg in messages.iter().skip(state.message_count_seen) {
            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Ok(s) = serde_json::to_string(&json!({
                    "type": "message",
                    "timestamp": ts,
                    "data": msg,
                })) {
                    entries.push(s);
                }
            }
        }
        state.message_count_seen = messages.len();
    });

    append_entries(&file_path, &entries);
}

/// Dump a response (or stream summary) entry.
pub fn dump_response_value(response: &Value, session_id: Option<&str>) {
    if !dump_enabled() {
        return;
    }
    let session = session_id
        .map(|s| s.to_string())
        .unwrap_or_else(crate::bootstrap::state::get_session_id);
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let file_path = get_dump_prompts_path(Some(&session));
    let data = if crate::services::api::api_trace::trace_level().is_full() {
        crate::services::api::api_trace::redact_value(response.clone())
    } else {
        summarize_response(response)
    };
    if let Ok(line) = serde_json::to_string(&json!({
        "type": "response",
        "timestamp": ts,
        "data": data,
    })) {
        append_entries(&file_path, &[line]);
    }
}

fn summarize_response(response: &Value) -> Value {
    if let Some(chunks) = response.get("chunks").and_then(|c| c.as_array()) {
        let types: Vec<String> = chunks
            .iter()
            .filter_map(|c| {
                c.get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        return json!({
            "stream": response.get("stream").cloned().unwrap_or(Value::Bool(true)),
            "chunk_count": chunks.len(),
            "event_types": types,
        });
    }
    json!({
        "keys": response.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
    })
}

/// Dump local (pre-SDK) messages when conversion fails.
pub fn dump_local_messages_on_error(local_messages: &[Value], error: &str) {
    if !dump_enabled() && !crate::services::api::api_trace::trace_level().is_enabled() {
        return;
    }
    if !dump_enabled() {
        return;
    }
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let file_path = get_dump_prompts_path(None);
    let payload = json!({
        "type": "local_messages_error",
        "timestamp": ts,
        "error": error,
        "messages": crate::services::api::api_trace::redact_value(Value::Array(local_messages.to_vec())),
    });
    if let Ok(line) = serde_json::to_string(&payload) {
        append_entries(&file_path, &[line]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keeps_last_five() {
        clear_api_request_cache();
        for i in 0..7 {
            add_api_request_to_cache(json!({ "n": i }));
        }
        let cached = get_last_api_requests();
        assert_eq!(cached.len(), 5);
        assert_eq!(cached[0].request["n"], 2);
        assert_eq!(cached[4].request["n"], 6);
        clear_api_request_cache();
    }
}
