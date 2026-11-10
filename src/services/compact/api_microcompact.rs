//! API-based microcompact that uses native context management.
//! Maps to: CC `services/compact/apiMicrocompact.ts`.

use serde_json::Value as JsonValue;

/// Maps to: CC `services/compact/apiMicrocompact.ts` `getAPIContextManagement(...)`.
pub fn get_api_context_management(
    has_thinking: bool,
    is_redact_thinking_active: bool,
    clear_all_thinking: bool,
) -> Option<JsonValue> {
    // Preserve thinking blocks in previous assistant turns. Skip when
    // redact-thinking is active — redacted blocks have no model-visible content.
    // When clearAllThinking is set (>1h idle = cache miss), keep only the last
    // thinking turn — the API schema requires value >= 1, and omitting the edit
    // falls back to the model-policy default (often "all"), which wouldn't clear.
    if !has_thinking || is_redact_thinking_active {
        return None;
    }
    let keep = if clear_all_thinking {
        serde_json::json!({ "type": "thinking_turns", "value": 1 })
    } else {
        serde_json::json!("all")
    };
    Some(serde_json::json!({
        "edits": [{
            "type": "clear_thinking_20251015",
            "keep": keep,
        }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_api_context_management_only_carries_the_thinking_edit() {
        assert_eq!(get_api_context_management(false, false, false), None);
        assert_eq!(get_api_context_management(true, true, false), None);
        assert_eq!(
            get_api_context_management(true, false, false),
            Some(serde_json::json!({
                "edits": [{"type": "clear_thinking_20251015", "keep": "all"}],
            }))
        );
        assert_eq!(
            get_api_context_management(true, false, true),
            Some(serde_json::json!({
                "edits": [{
                    "type": "clear_thinking_20251015",
                    "keep": {"type": "thinking_turns", "value": 1},
                }],
            }))
        );
    }
}
