//! SDK/internal message mappings.
//! Maps to: CC `utils/messages/mappers.ts:1-290`.
//!
//! These wire mappings retain raw JSON message payloads, as CC passes the API
//! message through unchanged. Typed consumers use the existing conversation
//! field adapter afterward; mapping policy is not duplicated there. SDK bridge
//! callers are not yet implemented; headless history and system output are live.

use crate::constants::xml::{LOCAL_COMMAND_STDERR_TAG, LOCAL_COMMAND_STDOUT_TAG};
use crate::services::claude_ai_limits::{ClaudeAiLimits, RateLimitType};
use serde_json::{Map, Value, json};

/// Maps to: CC `utils/messages/mappers.ts:26-74#toInternalMessages`.
pub(crate) fn to_internal_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| match message.get("type").and_then(Value::as_str) {
            Some("assistant") => Some(json!({
                "type": "assistant",
                "message": message["message"],
                "uuid": message["uuid"],
                "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            })),
            Some("user") => {
                let mut internal = json!({
                    "type": "user",
                    "message": message["message"],
                    "uuid": message.get("uuid").filter(|value| !value.is_null()).cloned()
                        .unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
                    "timestamp": message.get("timestamp").filter(|value| !value.is_null()).cloned()
                        .unwrap_or_else(|| json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))),
                });
                if let Some(is_synthetic) = message.get("isSynthetic") {
                    internal["isMeta"] = is_synthetic.clone();
                }
                Some(internal)
            }
            Some("system") if message["subtype"] == "compact_boundary" => Some(json!({
                "type": "system",
                "content": "Conversation compacted",
                "level": "info",
                "subtype": "compact_boundary",
                "compactMetadata": from_sdk_compact_metadata(&message["compact_metadata"]),
                "uuid": message["uuid"],
                "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            })),
            _ => None,
        })
        .collect()
}

/// Maps to: CC `utils/messages/mappers.ts:78-93#toSDKCompactMetadata`.
pub(crate) fn to_sdk_compact_metadata(meta: &Value) -> Value {
    let mut output = Map::new();
    for (internal, sdk) in [("trigger", "trigger"), ("preTokens", "pre_tokens")] {
        if let Some(value) = meta.get(internal) {
            output.insert(sdk.to_string(), value.clone());
        }
    }
    if let Some(segment) = meta
        .get("preservedSegment")
        .filter(|value| value.is_object())
    {
        let mut sdk_segment = Map::new();
        for (internal, sdk) in [
            ("headUuid", "head_uuid"),
            ("anchorUuid", "anchor_uuid"),
            ("tailUuid", "tail_uuid"),
        ] {
            if let Some(value) = segment.get(internal) {
                sdk_segment.insert(sdk.to_string(), value.clone());
            }
        }
        output.insert("preserved_segment".to_string(), sdk_segment.into());
    }
    output.into()
}

/// Maps to: CC `utils/messages/mappers.ts:98-113#fromSDKCompactMetadata`.
pub(crate) fn from_sdk_compact_metadata(meta: &Value) -> Value {
    let mut output = Map::new();
    for (sdk, internal) in [("trigger", "trigger"), ("pre_tokens", "preTokens")] {
        if let Some(value) = meta.get(sdk) {
            output.insert(internal.to_string(), value.clone());
        }
    }
    if let Some(segment) = meta
        .get("preserved_segment")
        .filter(|value| value.is_object())
    {
        let mut internal_segment = Map::new();
        for (sdk, internal) in [
            ("head_uuid", "headUuid"),
            ("anchor_uuid", "anchorUuid"),
            ("tail_uuid", "tailUuid"),
        ] {
            if let Some(value) = segment.get(sdk) {
                internal_segment.insert(internal.to_string(), value.clone());
            }
        }
        output.insert("preservedSegment".to_string(), internal_segment.into());
    }
    output.into()
}

/// Maps to: CC `utils/messages/mappers.ts:115-181#toSDKMessages`.
/// Unlike `queryHelpers.ts#normalizeMessage`, this does not split assistant
/// messages or wrap raw tool results in MCP metadata.
pub(crate) fn to_sdk_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| {
            match message.get("type").and_then(Value::as_str) {
                Some("assistant") => {
                    let mut sdk = json!({
                        "type": "assistant",
                        "message": normalize_assistant_message_for_sdk(message),
                        "session_id": crate::bootstrap::state::get_session_id(),
                        "parent_tool_use_id": null,
                        "uuid": message["uuid"],
                    });
                    if let Some(error) = message.get("error") {
                        sdk["error"] = error.clone();
                    }
                    Some(sdk)
                }
                Some("user") => {
                    let mut sdk = json!({
                        "type": "user",
                        "message": message["message"],
                        "session_id": crate::bootstrap::state::get_session_id(),
                        "parent_tool_use_id": null,
                        "uuid": message["uuid"],
                    });
                    if let Some(timestamp) = message.get("timestamp") {
                        sdk["timestamp"] = timestamp.clone();
                    }
                    // These source fields are optional booleans: `a || b` retains
                    // b's absence/null/false when a is false or absent.
                    if message.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        sdk["isSynthetic"] = json!(true);
                    } else if let Some(visible) = message.get("isVisibleInTranscriptOnly") {
                        sdk["isSynthetic"] = visible.clone();
                    }
                    if let Some(result) = message.get("toolUseResult") {
                        sdk["tool_use_result"] = result.clone();
                    }
                    Some(sdk)
                }
                Some("system") => {
                    if message["subtype"] == "compact_boundary" {
                        if let Some(meta) = message
                            .get("compactMetadata")
                            .filter(|value| value.is_object())
                        {
                            return Some(json!({
                                "type": "system",
                                "subtype": "compact_boundary",
                                "session_id": crate::bootstrap::state::get_session_id(),
                                "uuid": message["uuid"],
                                "compact_metadata": to_sdk_compact_metadata(meta),
                            }));
                        }
                    }
                    if message["subtype"] == "local_command" {
                        if let Some(content) = message.get("content").and_then(Value::as_str) {
                            if content.contains(&format!("<{LOCAL_COMMAND_STDOUT_TAG}>"))
                                || content.contains(&format!("<{LOCAL_COMMAND_STDERR_TAG}>"))
                            {
                                return Some(local_command_output_to_sdk_assistant_message(
                                    content,
                                    message["uuid"].as_str().unwrap_or_default(),
                                ));
                            }
                        }
                    }
                    None
                }
                _ => None,
            }
        })
        .collect()
}

/// Maps to: CC `utils/messages/mappers.ts:196-215#localCommandOutputToSDKAssistantMessage`.
pub(crate) fn local_command_output_to_sdk_assistant_message(
    raw_content: &str,
    uuid: &str,
) -> Value {
    // Carrier for strip-ansi: CSI, OSC (BEL/ST terminated), and two-byte ESC
    // control sequences. Unlike a color-only regex this also removes OSC links.
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:\x1b\]|\x{009d})[^\x07\x1b\x{009c}]*(?:\x07|\x1b\\|\x{009c})|(?:\x1b\[|\x{009b})[0-?]*[ -/]*[@-~]|\x1b[ -/]*[@-Z\\-_]").unwrap()
    });
    static STDOUT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<local-command-stdout>([\s\S]*?)</local-command-stdout>").unwrap()
    });
    static STDERR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<local-command-stderr>([\s\S]*?)</local-command-stderr>").unwrap()
    });
    let clean_content = ANSI.replace_all(raw_content, "");
    let clean_content = STDOUT.replace(&clean_content, "$1");
    let clean_content = STDERR.replace(&clean_content, "$1");
    let synthetic = super::create_assistant_message_value(
        clean_content
            .trim_matches(|character: char| {
                (character.is_whitespace() && character != '\u{85}') || character == '\u{feff}'
            })
            .to_string(),
    );
    json!({
        "type": "assistant",
        "message": synthetic["message"],
        "parent_tool_use_id": null,
        "session_id": crate::bootstrap::state::get_session_id(),
        "uuid": uuid,
    })
}

/// Maps to: CC `utils/messages/mappers.ts:221-252#toSDKRateLimitInfo`.
/// The existing typed quota carrier always supplies `is_using_overage`, while
/// the TS type also permits its absence. Other optional values remain omitted.
pub(crate) fn to_sdk_rate_limit_info(limits: Option<&ClaudeAiLimits>) -> Option<Value> {
    let limits = limits?;
    let mut info = Map::from_iter([
        ("status".to_string(), json!(limits.status.as_str())),
        ("isUsingOverage".to_string(), json!(limits.is_using_overage)),
    ]);
    if let Some(value) = limits.resets_at {
        info.insert("resetsAt".to_string(), json!(value));
    }
    if let Some(value) = limits.rate_limit_type {
        info.insert(
            "rateLimitType".to_string(),
            json!(match value {
                RateLimitType::FiveHour => "five_hour",
                RateLimitType::SevenDay => "seven_day",
                RateLimitType::SevenDayOpus => "seven_day_opus",
                RateLimitType::SevenDaySonnet => "seven_day_sonnet",
                RateLimitType::Overage => "overage",
            }),
        );
    }
    if let Some(value) = limits.utilization {
        info.insert("utilization".to_string(), json!(value));
    }
    if let Some(value) = limits.overage_status {
        info.insert("overageStatus".to_string(), json!(value.as_str()));
    }
    if let Some(value) = limits.overage_resets_at {
        info.insert("overageResetsAt".to_string(), json!(value));
    }
    if let Some(value) = &limits.overage_disabled_reason {
        info.insert("overageDisabledReason".to_string(), json!(value));
    }
    if let Some(value) = limits.surpassed_threshold {
        info.insert("surpassedThreshold".to_string(), json!(value));
    }
    Some(info.into())
}

/// Maps to: CC `utils/messages/mappers.ts:260-290#normalizeAssistantMessageForSDK`.
fn normalize_assistant_message_for_sdk(message: &Value) -> Value {
    let mut api_message = message["message"].clone();
    let Some(content) = api_message.get_mut("content").and_then(Value::as_array_mut) else {
        return api_message;
    };
    for block in content {
        if block["type"] != "tool_use" {
            continue;
        }
        if block["name"]
            == crate::tools::exit_plan_mode_tool::constants::EXIT_PLAN_MODE_V2_TOOL_NAME
        {
            if let Some(plan) = crate::utils::plans::get_plan(None).filter(|plan| !plan.is_empty())
            {
                let mut input = block
                    .get("input")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                input.insert("plan".to_string(), json!(plan));
                block["input"] = input.into();
            }
        }
    }
    api_message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{IsolatedProjectSettings, TEST_ENV_LOCK};

    #[test]
    fn sdk_history_envelopes_match_official_field_selection() {
        let assistant = json!({
            "type":"assistant", "uuid":"assistant-uuid", "timestamp":"2000-01-01T00:00:00Z",
            "requestId":"must-not-replay", "error":"must-not-replay",
            "message":{"id":"api-id","content":[{"type":"future_block","data":{"x":1}}],"future":true}
        });
        let user = json!({
            "type":"user", "uuid":"user-uuid", "timestamp":"2001-01-01T00:00:00Z",
            "isSynthetic":true, "tool_use_result":{"discard":"source does not copy this"},
            "message":{"role":"user","content":"history","future":42}
        });
        let result = to_internal_messages(&[
            assistant.clone(),
            user.clone(),
            json!({"type":"system","subtype":"init"}),
            json!({"type":"result"}),
            json!({"type":"progress"}),
        ]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["message"], assistant["message"]);
        assert_eq!(result[0]["uuid"], "assistant-uuid");
        assert_ne!(result[0]["timestamp"], assistant["timestamp"]);
        assert!(result[0].get("requestId").is_none());
        assert!(result[0].get("error").is_none());
        assert_eq!(
            result[1],
            json!({
                "type":"user", "uuid":"user-uuid", "timestamp":"2001-01-01T00:00:00Z",
                "isMeta":true, "message":user["message"],
            })
        );
    }

    #[test]
    fn sdk_history_nullish_user_defaults_match_official() {
        let result = to_internal_messages(&[
            json!({"type":"user","message":{"content":"one"}}),
            json!({"type":"user","uuid":null,"timestamp":null,"isSynthetic":false,"message":{"content":"two"}}),
            json!({"type":"user","uuid":"","timestamp":"","message":{"content":"three"}}),
        ]);
        for message in &result[..2] {
            uuid::Uuid::parse_str(message["uuid"].as_str().unwrap()).unwrap();
            chrono::DateTime::parse_from_rfc3339(message["timestamp"].as_str().unwrap()).unwrap();
        }
        assert_ne!(result[0]["uuid"], result[1]["uuid"]);
        assert!(result[0].get("isMeta").is_none());
        assert_eq!(result[1]["isMeta"], false);
        assert_eq!(result[2]["uuid"], "");
        assert_eq!(result[2]["timestamp"], "");
    }

    #[test]
    fn compact_metadata_roundtrip_matches_official_projection() {
        let internal = json!({
            "trigger":"manual", "preTokens":0,
            "preservedSegment":{"headUuid":"h","anchorUuid":"a","tailUuid":"t","drop":true},
            "preCompactDiscoveredTools":["Read"], "drop":true,
        });
        let sdk = to_sdk_compact_metadata(&internal);
        assert_eq!(
            sdk,
            json!({
                "trigger":"manual","pre_tokens":0,
                "preserved_segment":{"head_uuid":"h","anchor_uuid":"a","tail_uuid":"t"},
            })
        );
        assert_eq!(
            from_sdk_compact_metadata(&sdk),
            json!({
                "trigger":"manual","preTokens":0,
                "preservedSegment":{"headUuid":"h","anchorUuid":"a","tailUuid":"t"},
            })
        );
        assert_eq!(
            to_sdk_compact_metadata(
                &json!({"trigger":"auto","preTokens":9,"preservedSegment":null})
            ),
            json!({"trigger":"auto","pre_tokens":9})
        );
        assert_eq!(
            from_sdk_compact_metadata(&json!({"trigger":"auto","pre_tokens":9})),
            json!({"trigger":"auto","preTokens":9})
        );
        let boundary = to_internal_messages(&[json!({
            "type":"system","subtype":"compact_boundary","uuid":"boundary",
            "compact_metadata":sdk,
        })]);
        assert_eq!(boundary[0]["content"], "Conversation compacted");
        assert_eq!(boundary[0]["level"], "info");
        assert_eq!(
            boundary[0]["compactMetadata"]["preservedSegment"]["anchorUuid"],
            "a"
        );
        let output = to_sdk_messages(&boundary);
        assert_eq!(output[0]["compact_metadata"], sdk);
        assert_eq!(output[0]["uuid"], "boundary");
    }

    #[test]
    fn sdk_messages_match_official_raw_results_errors_and_synthetic_semantics() {
        let assistant = json!({"type":"assistant","uuid":"a","error":"overloaded","message":{"content":"raw","extra":42}});
        let user = json!({"type":"user","uuid":"u","timestamp":"time","isMeta":true,
            "mcpMeta":{"serverName":"must-not-wrap"},"toolUseResult":{"output":1},
            "message":{"role":"user","content":[{"type":"future_user_block","data":1}]}});
        let values = to_sdk_messages(&[
            assistant.clone(),
            user.clone(),
            json!({"type":"user","uuid":"absent","isMeta":false,"message":{"content":"x"}}),
            json!({"type":"user","uuid":"false","isVisibleInTranscriptOnly":false,"toolUseResult":null,"message":{"content":"y"}}),
            json!({"type":"user","uuid":"visible","isVisibleInTranscriptOnly":true,"message":{"content":"z"}}),
            json!({"type":"attachment"}),
            json!({"type":"progress"}),
            json!({"type":"system","subtype":"compact_boundary"}),
            json!({"type":"system","subtype":"local_command","content":"<command-name>/cost</command-name>"}),
        ]);
        assert_eq!(values.len(), 5);
        assert_eq!(values[0]["error"], "overloaded");
        assert_eq!(values[0]["message"], assistant["message"]);
        assert_eq!(values[1]["message"], user["message"]);
        assert_eq!(values[1]["tool_use_result"], json!({"output":1}));
        assert_eq!(values[1]["isSynthetic"], true);
        assert!(values[2].get("isSynthetic").is_none());
        assert!(values[2].get("tool_use_result").is_none());
        assert_eq!(values[3]["isSynthetic"], false);
        assert_eq!(values[3].get("tool_use_result"), Some(&Value::Null));
        assert_eq!(values[4]["isSynthetic"], true);
        assert!(
            values
                .iter()
                .all(|message| message["parent_tool_use_id"].is_null())
        );
    }

    #[test]
    fn local_output_matches_official_complete_synthetic_envelope_and_first_wrappers() {
        let raw = " \u{1b}[2m<local-command-stdout>first\nline</local-command-stdout>\u{1b}[0m\n<local-command-stdout>second</local-command-stdout>\n<local-command-stderr>err</local-command-stderr> ";
        let value = local_command_output_to_sdk_assistant_message(raw, "original-uuid");
        assert_eq!(value["uuid"], "original-uuid");
        assert_eq!(
            value["message"]["content"][0]["text"],
            "first\nline\n<local-command-stdout>second</local-command-stdout>\nerr"
        );
        assert_eq!(value["message"]["model"], "<synthetic>");
        assert_eq!(value["message"]["type"], "message");
        assert_eq!(value["message"]["role"], "assistant");
        assert_eq!(value["message"]["stop_reason"], "stop_sequence");
        assert_eq!(value["message"]["stop_sequence"], "");
        assert_eq!(value["message"]["usage"]["input_tokens"], 0);
        assert_eq!(
            value["message"]["usage"]["server_tool_use"]["web_fetch_requests"],
            0
        );
        uuid::Uuid::parse_str(value["message"]["id"].as_str().unwrap()).unwrap();
        assert!(value.get("error").is_none());
        assert_eq!(
            local_command_output_to_sdk_assistant_message("\u{feff} text \u{feff}", "bom")["message"]
                ["content"][0]["text"],
            "text"
        );
        assert_eq!(
            local_command_output_to_sdk_assistant_message("\u{85}text\u{85}", "next-line")["message"]
                ["content"][0]["text"],
            "\u{85}text\u{85}"
        );
        let empty = local_command_output_to_sdk_assistant_message(
            "<local-command-stdout> </local-command-stdout>",
            "empty",
        );
        assert_eq!(
            empty["message"]["content"][0]["text"],
            super::super::NO_CONTENT_MESSAGE
        );
        for output in [
            "\u{1b}]8;;https://example.test\u{7}link\u{1b}]8;;\u{7}",
            "\u{1b}]8;;https://example.test\u{1b}\\link\u{1b}]8;;\u{1b}\\",
        ] {
            assert_eq!(
                local_command_output_to_sdk_assistant_message(output, "link")["message"]["content"]
                    [0]["text"],
                "link"
            );
        }
    }

    #[test]
    fn sdk_rate_limits_match_official_falsey_values_and_private_field_filter() {
        assert_eq!(to_sdk_rate_limit_info(None), None);
        let limits = ClaudeAiLimits {
            unified_rate_limit_fallback_available: true,
            resets_at: Some(0.0),
            utilization: Some(0.0),
            rate_limit_type: Some(RateLimitType::SevenDayOpus),
            overage_status: Some(crate::services::claude_ai_limits::QuotaStatus::AllowedWarning),
            overage_resets_at: Some(0.0),
            overage_disabled_reason: Some(String::new()),
            surpassed_threshold: Some(0.0),
            ..Default::default()
        };
        let value = to_sdk_rate_limit_info(Some(&limits)).unwrap();
        assert_eq!(
            value,
            json!({
                "status":"allowed","isUsingOverage":false,"resetsAt":0.0,
                "utilization":0.0,"rateLimitType":"seven_day_opus","overageStatus":"allowed_warning",
                "overageResetsAt":0.0,"overageDisabledReason":"","surpassedThreshold":0.0,
            })
        );
    }

    #[test]
    fn sdk_exit_plan_input_matches_official_file_injection_without_mutating_internal() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _project = IsolatedProjectSettings::pin();
        let previous_session = crate::bootstrap::state::get_session_id();
        let session = uuid::Uuid::new_v4().to_string();
        crate::bootstrap::state::set_session_id(&session);
        let path = crate::utils::plans::get_plan_file_path(None);
        let message = json!({"type":"assistant","uuid":"a","message":{"content":[
            {"type":"text","text":"before"},
            {"type":"tool_use","id":"tool","name":"ExitPlanMode","input":{"plan":"old","allowedPrompts":[]}},
            {"type":"server_tool_use","id":"server","name":"ExitPlanMode","input":{"plan":"server"}},
            {"type":"tool_use","id":"other","name":"Read","input":{"file_path":"file"}}
        ],"extra":1}});
        assert_eq!(
            to_sdk_messages(std::slice::from_ref(&message))[0]["message"],
            message["message"]
        );
        std::fs::write(&path, "Plan from disk\n").unwrap();
        let output = to_sdk_messages(std::slice::from_ref(&message));
        assert_eq!(
            output[0]["message"]["content"][1]["input"],
            json!({"plan":"Plan from disk\n","allowedPrompts":[]})
        );
        assert_eq!(message["message"]["content"][1]["input"]["plan"], "old");
        assert_eq!(
            output[0]["message"]["content"][2],
            message["message"]["content"][2]
        );
        assert_eq!(
            output[0]["message"]["content"][3],
            message["message"]["content"][3]
        );
        std::fs::write(&path, "").unwrap();
        assert_eq!(
            to_sdk_messages(std::slice::from_ref(&message))[0]["message"],
            message["message"]
        );
        std::fs::remove_file(path).unwrap();
        crate::bootstrap::state::set_session_id(previous_session);
        crate::utils::plans::clear_plan_slug(Some(&session));
    }
}
