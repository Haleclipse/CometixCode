//! API-based microcompact that uses native context management.
//! Maps to: CC `services/compact/apiMicrocompact.ts`.

use crate::tools::file_edit_tool::constants::FILE_EDIT_TOOL_NAME;
use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
use crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME;
use crate::tools::glob_tool::prompt::GLOB_TOOL_NAME;
use crate::tools::grep_tool::prompt::GREP_TOOL_NAME;
use crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME;
use crate::tools::web_fetch_tool::prompt::WEB_FETCH_TOOL_NAME;
use crate::tools::web_search_tool::prompt::WEB_SEARCH_TOOL_NAME;
use crate::utils::shell::shell_tool_utils::SHELL_TOOL_NAMES;
use serde_json::Value as JsonValue;

/// Maps to: CC `services/compact/apiMicrocompact.ts` `DEFAULT_MAX_INPUT_TOKENS`.
const DEFAULT_MAX_INPUT_TOKENS: i64 = 180_000;
/// Maps to: CC `services/compact/apiMicrocompact.ts` `DEFAULT_TARGET_INPUT_TOKENS`.
const DEFAULT_TARGET_INPUT_TOKENS: i64 = 40_000;

/// Maps to: CC `services/compact/apiMicrocompact.ts` `TOOLS_CLEARABLE_RESULTS`.
fn tools_clearable_results() -> Vec<&'static str> {
    let mut names = SHELL_TOOL_NAMES.to_vec();
    names.extend([
        GLOB_TOOL_NAME,
        GREP_TOOL_NAME,
        FILE_READ_TOOL_NAME,
        WEB_FETCH_TOOL_NAME,
        WEB_SEARCH_TOOL_NAME,
    ]);
    names
}

/// Maps to: CC `services/compact/apiMicrocompact.ts` `TOOLS_CLEARABLE_USES`.
const TOOLS_CLEARABLE_USES: &[&str] = &[
    FILE_EDIT_TOOL_NAME,
    FILE_WRITE_TOOL_NAME,
    NOTEBOOK_EDIT_TOOL_NAME,
];

/// Maps to: CC `services/compact/apiMicrocompact.ts` `getAPIContextManagement(...)`.
pub fn get_api_context_management(
    has_thinking: bool,
    is_redact_thinking_active: bool,
    clear_all_thinking: bool,
) -> Option<JsonValue> {
    let mut edits = Vec::new();

    // Preserve thinking blocks in previous assistant turns. Skip when
    // redact-thinking is active — redacted blocks have no model-visible content.
    // When clearAllThinking is set (>1h idle = cache miss), keep only the last
    // thinking turn — the API schema requires value >= 1, and omitting the edit
    // falls back to the model-policy default (often "all"), which wouldn't clear.
    if has_thinking && !is_redact_thinking_active {
        let keep = if clear_all_thinking {
            serde_json::json!({ "type": "thinking_turns", "value": 1 })
        } else {
            serde_json::json!("all")
        };
        edits.push(serde_json::json!({
            "type": "clear_thinking_20251015",
            "keep": keep,
        }));
    }

    // Tool clearing strategies are ant-only.
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        return if edits.is_empty() {
            None
        } else {
            Some(serde_json::json!({ "edits": edits }))
        };
    }

    let use_clear_tool_results = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("USE_API_CLEAR_TOOL_RESULTS").as_deref(),
    );
    let use_clear_tool_uses = crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("USE_API_CLEAR_TOOL_USES").as_deref(),
    );
    if !use_clear_tool_results && !use_clear_tool_uses {
        return if edits.is_empty() {
            None
        } else {
            Some(serde_json::json!({ "edits": edits }))
        };
    }

    let trigger_threshold = crate::utils::process_env::var("API_MAX_INPUT_TOKENS")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_MAX_INPUT_TOKENS);
    let keep_target = crate::utils::process_env::var("API_TARGET_INPUT_TOKENS")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_TARGET_INPUT_TOKENS);
    let clear_at_least = trigger_threshold - keep_target;

    if use_clear_tool_results {
        edits.push(serde_json::json!({
            "type": "clear_tool_uses_20250919",
            "trigger": { "type": "input_tokens", "value": trigger_threshold },
            "clear_at_least": { "type": "input_tokens", "value": clear_at_least },
            "clear_tool_inputs": tools_clearable_results(),
        }));
    }
    if use_clear_tool_uses {
        edits.push(serde_json::json!({
            "type": "clear_tool_uses_20250919",
            "trigger": { "type": "input_tokens", "value": trigger_threshold },
            "clear_at_least": { "type": "input_tokens", "value": clear_at_least },
            "exclude_tools": TOOLS_CLEARABLE_USES,
        }));
    }

    if edits.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "edits": edits }))
    }
}

#[cfg(all(test, feature = "anthropic_internal"))]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn get_api_context_management_tool_clearing_matches_official_env_shape() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("USE_API_CLEAR_TOOL_RESULTS", "1");
        crate::utils::process_env::set("USE_API_CLEAR_TOOL_USES", "1");
        crate::utils::process_env::set("API_MAX_INPUT_TOKENS", "100");
        crate::utils::process_env::set("API_TARGET_INPUT_TOKENS", "150");

        let context_management = get_api_context_management(false, false, false)
            .expect("tool clearing env should produce context management");
        let edits = context_management["edits"].as_array().expect("edits array");

        assert_eq!(edits.len(), 2);
        assert_eq!(
            edits[0]["clear_at_least"],
            serde_json::json!({ "type": "input_tokens", "value": -50 })
        );
        assert_eq!(
            edits[0]["clear_tool_inputs"],
            serde_json::json!([
                "Bash",
                "PowerShell",
                "Glob",
                "Grep",
                "Read",
                "WebFetch",
                "WebSearch"
            ])
        );
        assert_eq!(
            edits[1]["clear_at_least"],
            serde_json::json!({ "type": "input_tokens", "value": -50 })
        );
        assert_eq!(
            edits[1]["exclude_tools"],
            serde_json::json!(["Edit", "Write", "NotebookEdit"])
        );

        crate::utils::process_env::remove("USE_API_CLEAR_TOOL_RESULTS");
        crate::utils::process_env::remove("USE_API_CLEAR_TOOL_USES");
        crate::utils::process_env::remove("API_MAX_INPUT_TOKENS");
        crate::utils::process_env::remove("API_TARGET_INPUT_TOKENS");
    }
}
