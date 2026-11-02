//! Hook JSON output parsing.
//! Maps to: CC utils/hooks.ts lines 382-640 (validateHookJson, parseHookOutput,
//! parseHttpHookOutput, processHookJSONOutput).
//!
//! Hook commands write JSON to stdout. This module parses and validates that
//! output into typed HookResult fields.

use super::{
    HookBlockingError, HookElicitationResponse, HookOutcome, HookResult, PermissionBehavior,
};
// The decision union is types/hooks.ts material (#162); utils/hooks.ts imports
// it the same way (:66-75).
use crate::types::hooks::PermissionRequestResult;
use serde::Deserialize;

/// Raw JSON output from a hook command.
/// Maps to: CC `HookJSONOutput` (sync variant).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct HookJsonOutput {
    /// false = stop the query loop after this hook.
    #[serde(rename = "continue")]
    pub should_continue: Option<bool>,
    /// Suppress stdout from being shown as a system message.
    pub suppress_output: Option<bool>,
    /// Reason for stopping.
    pub stop_reason: Option<String>,
    /// 'approve' or 'block' — top-level permission decision.
    pub decision: Option<String>,
    /// Reason for the decision.
    pub reason: Option<String>,
    /// System message to display.
    pub system_message: Option<String>,
    /// Async execution flag.
    #[serde(rename = "async")]
    pub is_async: Option<bool>,
    /// Async hook timeout in milliseconds.
    /// Maps to: CC `AsyncHookJSONOutput.asyncTimeout` consumed by
    /// `utils/hooks/AsyncHookRegistry.ts#registerPendingAsyncHook`.
    pub async_timeout: Option<u64>,
    /// Hook-event-specific output.
    pub hook_specific_output: Option<HookSpecificOutput>,
}

/// Event-specific output fields.
/// Maps to: CC `HookJSONOutput.hookSpecificOutput`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct HookSpecificOutput {
    pub hook_event_name: Option<String>,
    pub permission_decision: Option<String>,
    pub permission_decision_reason: Option<String>,
    pub additional_context: Option<String>,
    pub initial_user_message: Option<String>,
    pub updated_input: Option<serde_json::Value>,
    pub updated_mcp_tool_output: Option<serde_json::Value>,
    pub watch_paths: Option<Vec<String>>,
    /// Elicitation action (accept/decline/cancel).
    pub action: Option<String>,
    /// Elicitation response content.
    pub content: Option<serde_json::Value>,
    /// PermissionRequest hook allow/deny decision.
    pub decision: Option<serde_json::Value>,
    /// Updated permission rules from PermissionRequest hooks.
    pub updated_permissions: Option<serde_json::Value>,
    /// Whether the hook wants a retry.
    pub retry: Option<bool>,
    /// WorktreeCreate hook worktree path (callback hooks, hooks.ts:3117-3121).
    pub worktree_path: Option<String>,
}

/// Parse hook stdout into structured output.
/// Maps to: CC `parseHookOutput()` (hooks.ts:399-450).
///
/// If stdout doesn't start with '{', treat as plain text system message.
/// Otherwise parse as JSON and validate.
pub fn parse_hook_output(stdout: &str) -> ParsedHookOutput {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return ParsedHookOutput::Empty;
    }
    if !trimmed.starts_with('{') {
        crate::utils::debug::log_for_debugging(
            "Hook output does not start with {, treating as plain text",
        );
        return ParsedHookOutput::PlainText(stdout.to_string());
    }

    // CC distinguishes JSON syntax failure from schema validation failure.
    // `jsonParse(...)` throwing falls back to ordinary plain text, so exit 2
    // still follows the non-JSON blocking convention. Only syntactically valid
    // JSON that fails the hook schema takes the non-blocking validation path.
    let value = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => value,
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Failed to parse hook output as JSON: {error}"
            ));
            return ParsedHookOutput::PlainText(stdout.to_string());
        }
    };
    // Maps to: CC `validateHookJson` — hookJSONOutputSchema().safeParse; the
    // serde struct projects from the validated data.
    match crate::utils::zod::safe_parse(crate::types::hooks::hook_json_output_schema(), &value) {
        Ok(data) => {
            crate::utils::debug::log_for_debugging(
                "Successfully parsed and validated hook JSON output",
            );
            match serde_json::from_value::<HookJsonOutput>(data) {
                Ok(json) => ParsedHookOutput::Json(json),
                Err(error) => {
                    // The projection lagging the schema is a porting bug, not
                    // a user error; degrade like a validation failure.
                    ParsedHookOutput::ValidationError {
                        plain_text: stdout.to_string(),
                        error: format!("Invalid hook JSON: {error}"),
                    }
                }
            }
        }
        Err(zod_error) => {
            // CC's three-part copy (hooks.ts:391-446): the per-issue list,
            // the echoed output, and — for command hooks — the schema hint.
            let issues = zod_error
                .issues
                .iter()
                .map(|issue| {
                    let path = issue
                        .path
                        .iter()
                        .map(|segment| match segment {
                            crate::utils::zod::PathSegment::Key(key) => key.clone(),
                            crate::utils::zod::PathSegment::Index(index) => index.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(".");
                    format!("  - {path}: {}", issue.message)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let echoed = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            let validation_error = format!(
                "Hook JSON output validation failed:\n{issues}\n\nThe hook's output was: {echoed}"
            );
            let error = format!(
                "{validation_error}\n\nExpected schema:\n{}",
                expected_schema_hint()
            );
            crate::utils::debug::log_for_debugging(&error);
            ParsedHookOutput::ValidationError {
                plain_text: stdout.to_string(),
                error,
            }
        }
    }
}

/// Maps to: CC `parseHookOutput`'s schema hint (hooks.ts:414-446) — the fixed
/// object stringified with two-space indentation, verbatim.
fn expected_schema_hint() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "continue": "boolean (optional)",
        "suppressOutput": "boolean (optional)",
        "stopReason": "string (optional)",
        "decision": "\"approve\" | \"block\" (optional)",
        "reason": "string (optional)",
        "systemMessage": "string (optional)",
        "permissionDecision": "\"allow\" | \"deny\" | \"ask\" (optional)",
        "hookSpecificOutput": {
            "for PreToolUse": {
                "hookEventName": "\"PreToolUse\"",
                "permissionDecision": "\"allow\" | \"deny\" | \"ask\" (optional)",
                "permissionDecisionReason": "string (optional)",
                "updatedInput": "object (optional) - Modified tool input to use",
            },
            "for UserPromptSubmit": {
                "hookEventName": "\"UserPromptSubmit\"",
                "additionalContext": "string (required)",
            },
            "for PostToolUse": {
                "hookEventName": "\"PostToolUse\"",
                "additionalContext": "string (optional)",
            },
        },
    }))
    .unwrap_or_default()
}

/// Parsed hook output variants.
pub enum ParsedHookOutput {
    Empty,
    PlainText(String),
    Json(HookJsonOutput),
    ValidationError { plain_text: String, error: String },
}

/// Process event-specific JSON only when its discriminator matches the hook
/// currently being executed. Upstream throws on this mismatch; the Rust hook
/// runner represents that throw as a non-blocking hook error and, critically,
/// applies none of the mismatched permission/input fields.
pub fn process_hook_json_output_for_event(
    json: &HookJsonOutput,
    command: &str,
    expected_hook_event: &str,
) -> HookResult {
    if let Some(specific) = json.hook_specific_output.as_ref()
        && specific.hook_event_name.as_deref() != Some(expected_hook_event)
    {
        return HookResult {
            outcome: HookOutcome::NonBlockingError,
            system_message: Some(format!(
                "Hook output error: Hook returned incorrect event name: expected '{expected_hook_event}' but got '{}'",
                specific.hook_event_name.as_deref().unwrap_or("missing")
            )),
            ..HookResult::default()
        };
    }
    process_hook_json_output(json, command)
}

/// Process validated JSON output into a HookResult.
/// Maps to: CC `utils/hooks.ts:489-742#processHookJSONOutput`.
/// JSON processing does not classify command completion: both command and
/// callback owners emit `outcome: success` for valid sync JSON (:2609, :4887).
pub fn process_hook_json_output(json: &HookJsonOutput, command: &str) -> HookResult {
    let mut result = HookResult::default();

    // continue === false → prevent continuation
    if json.should_continue == Some(false) {
        result.prevent_continuation = true;
        result.stop_reason = json.stop_reason.clone().filter(|reason| !reason.is_empty());
    }

    // Top-level decision
    match json.decision.as_deref() {
        Some("approve") => {
            result.permission_behavior = Some(PermissionBehavior::Allow);
        }
        Some("block") => {
            result.permission_behavior = Some(PermissionBehavior::Deny);
            result.blocking_error = Some(HookBlockingError {
                blocking_error: json
                    .reason
                    .clone()
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or_else(|| "Blocked by hook".to_string()),
                command: command.to_string(),
            });
        }
        _ => {}
    }

    if result.permission_behavior.is_some() {
        result.hook_permission_decision_reason = json.reason.clone();
    }

    // CC tests systemMessage for JavaScript truthiness.
    if let Some(msg) = json
        .system_message
        .as_ref()
        .filter(|message| !message.is_empty())
    {
        result.system_message = Some(msg.clone());
    }

    // Hook-specific output
    if let Some(ref specific) = json.hook_specific_output {
        if let Some(ref ctx) = specific.additional_context {
            result.additional_context = Some(ctx.clone());
        }
        if let Some(ref msg) = specific.initial_user_message {
            result.initial_user_message = Some(msg.clone());
        }
        if let Some(ref input) = specific.updated_input {
            result.updated_input = Some(input.clone());
        }

        // PreToolUse permission decision override
        match specific.permission_decision.as_deref() {
            Some("allow") => {
                result.permission_behavior = Some(PermissionBehavior::Allow);
                result.hook_permission_decision_reason =
                    specific.permission_decision_reason.clone();
            }
            Some("deny") => {
                result.permission_behavior = Some(PermissionBehavior::Deny);
                result.blocking_error = Some(HookBlockingError {
                    blocking_error: specific
                        .permission_decision_reason
                        .clone()
                        .filter(|reason| !reason.is_empty())
                        .or_else(|| json.reason.clone().filter(|reason| !reason.is_empty()))
                        .unwrap_or_else(|| "Blocked by hook".to_string()),
                    command: command.to_string(),
                });
            }
            Some("ask") => {
                result.permission_behavior = Some(PermissionBehavior::Ask);
            }
            _ => {}
        }

        // CC :631-632 assigns this even when the specific reason is absent;
        // it replaces the top-level reason for every PreToolUse-specific output.
        if specific.hook_event_name.as_deref() == Some("PreToolUse") {
            result.hook_permission_decision_reason = specific.permission_decision_reason.clone();
        }

        // CC `hooks.ts:657-672` — the whole `decision` object is kept on the
        // result (it is the only carrier for the deny arm's `message` and
        // `interrupt`), `permissionBehavior` mirrors its `behavior`, and the
        // allow arm's `updatedInput` is flattened onto the result.
        if let Some(decision) = specific
            .decision
            .as_ref()
            .and_then(PermissionRequestResult::from_decision_json)
        {
            match &decision {
                PermissionRequestResult::Allow {
                    updated_input,
                    updated_permissions,
                } => {
                    result.permission_behavior = Some(PermissionBehavior::Allow);
                    if let Some(updated_input) = updated_input {
                        result.updated_input = Some(updated_input.clone());
                    }
                    result.permission_updates = updated_permissions.clone();
                }
                PermissionRequestResult::Deny { .. } => {
                    result.permission_behavior = Some(PermissionBehavior::Deny);
                }
            }
            result.permission_request_result = Some(decision);
        }

        // Additional event-specific fields
        if let Some(ref output) = specific.updated_mcp_tool_output {
            result.updated_mcp_tool_output = Some(output.clone());
        }
        if let Some(ref paths) = specific.watch_paths {
            result.watch_paths = Some(paths.clone());
        }
        if let Some(retry) = specific.retry {
            result.retry = Some(retry);
        }

        match specific.hook_event_name.as_deref() {
            Some("Elicitation") => {
                if let Some(action) = specific.action.clone() {
                    result.elicitation_response = Some(HookElicitationResponse {
                        action: action.clone(),
                        content: specific.content.clone(),
                    });
                    if action == "decline" {
                        result.blocking_error = Some(HookBlockingError {
                            blocking_error: json
                                .reason
                                .clone()
                                .filter(|reason| !reason.is_empty())
                                .unwrap_or_else(|| "Elicitation denied by hook".to_string()),
                            command: command.to_string(),
                        });
                    }
                }
            }
            Some("ElicitationResult") => {
                if let Some(action) = specific.action.clone() {
                    result.elicitation_result_response = Some(HookElicitationResponse {
                        action: action.clone(),
                        content: specific.content.clone(),
                    });
                    if action == "decline" {
                        result.blocking_error = Some(HookBlockingError {
                            blocking_error: json
                                .reason
                                .clone()
                                .filter(|reason| !reason.is_empty())
                                .unwrap_or_else(|| {
                                    "Elicitation result blocked by hook".to_string()
                                }),
                            command: command.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps to: CC `validateHookJson` + `parseHookOutput` — schema-invalid
    /// output carries the three-part copy: the per-issue list, the echoed
    /// output, and the fixed schema hint.
    #[test]
    fn schema_invalid_output_reports_the_three_part_copy_like_official() {
        let ParsedHookOutput::ValidationError { plain_text, error } =
            parse_hook_output(r#"{"decision": "maybe"}"#)
        else {
            panic!("schema-invalid JSON must take the validation-error path");
        };
        assert_eq!(plain_text, r#"{"decision": "maybe"}"#);
        assert!(error.starts_with("Hook JSON output validation failed:\n  - "));
        assert!(error.contains("\n\nThe hook's output was: {\n  \"decision\": \"maybe\"\n}"));
        assert!(error.contains("\n\nExpected schema:\n{\n  \"continue\": \"boolean (optional)\""));
        assert!(error.contains("\"for UserPromptSubmit\""));

        // Valid sync and async shapes still validate through the carrier.
        assert!(matches!(
            parse_hook_output(r#"{"decision": "approve", "reason": "ok"}"#),
            ParsedHookOutput::Json(_)
        ));
        assert!(matches!(
            parse_hook_output(r#"{"async": true, "asyncTimeout": 5000}"#),
            ParsedHookOutput::Json(json) if json.is_async == Some(true)
        ));
        // A hookSpecificOutput arm projects through to the serde struct.
        let ParsedHookOutput::Json(json) = parse_hook_output(
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}}"#,
        ) else {
            panic!("valid PreToolUse output parses");
        };
        assert_eq!(
            json.hook_specific_output
                .as_ref()
                .and_then(|output| output.permission_decision.as_deref()),
            Some("allow")
        );
    }

    #[test]
    fn malformed_json_falls_back_to_plain_text_like_official() {
        let ParsedHookOutput::PlainText(text) = parse_hook_output("{\n") else {
            panic!("syntax-invalid JSON must use the plain-text fallback");
        };
        assert_eq!(text, "{\n");
    }

    #[test]
    fn schema_invalid_json_remains_a_validation_error_like_official() {
        assert!(matches!(
            parse_hook_output(r#"{"continue":"not-a-boolean"}"#),
            ParsedHookOutput::ValidationError { .. }
        ));
    }

    #[test]
    fn process_hook_json_output_extracts_elicitation_response_like_official() {
        let parsed = parse_hook_output(
            r#"{
                "hookSpecificOutput": {
                    "hookEventName": "Elicitation",
                    "action": "accept",
                    "content": {"email": "user@example.com"}
                }
            }"#,
        );
        let ParsedHookOutput::Json(json) = parsed else {
            panic!("expected hook json");
        };
        let result = process_hook_json_output(&json, "echo hook");
        assert_eq!(
            result.elicitation_response,
            Some(HookElicitationResponse {
                action: "accept".to_string(),
                content: Some(serde_json::json!({"email": "user@example.com"})),
            })
        );
        assert!(result.blocking_error.is_none());
    }

    /// Maps to: CC `hooks.ts:657-672` — a PermissionRequest hook's decision is
    /// kept WHOLE on `permissionRequestResult`, and `permissionDecisionReason`
    /// is not part of that arm (`types/hooks.ts:120-133`): the schema strips it,
    /// so `HookResult.hookPermissionDecisionReason` stays undefined here.
    ///
    /// This is the dead read the headless deny arm used to take its message
    /// from — it could only ever see `None`, so every hook deny reported the
    /// `'Permission denied by hook'` fallback with no reason. The deny arm's
    /// `interrupt` had the same problem one level up: carried as untyped JSON
    /// that nothing destructured.
    #[test]
    fn permission_request_deny_carries_message_and_interrupt_only_on_the_decision() {
        let ParsedHookOutput::Json(json) = parse_hook_output(
            r#"{
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "permissionDecisionReason": "stripped by the schema",
                    "decision": {
                        "behavior": "deny",
                        "message": "blocked by policy",
                        "interrupt": true
                    }
                }
            }"#,
        ) else {
            panic!("a PermissionRequest deny is valid hook output");
        };
        let result = process_hook_json_output(&json, "echo hook");
        assert_eq!(result.permission_behavior, Some(PermissionBehavior::Deny));
        assert_eq!(
            result.hook_permission_decision_reason, None,
            "permissionDecisionReason belongs to the PreToolUse arm and is stripped here"
        );
        assert_eq!(
            result.permission_request_result,
            Some(PermissionRequestResult::Deny {
                message: Some("blocked by policy".to_string()),
                interrupt: Some(true),
            })
        );

        // The allow arm's projections stay where the port's consumers read
        // them, and the decision keeps its own copy.
        let ParsedHookOutput::Json(json) = parse_hook_output(
            r#"{
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedInput": {"command": "echo hooked"},
                        "updatedPermissions": [
                            {"type": "setMode", "mode": "acceptEdits", "destination": "session"}
                        ]
                    }
                }
            }"#,
        ) else {
            panic!("a PermissionRequest allow is valid hook output");
        };
        let result = process_hook_json_output(&json, "echo hook");
        assert_eq!(result.permission_behavior, Some(PermissionBehavior::Allow));
        assert_eq!(
            result.updated_input,
            Some(serde_json::json!({"command": "echo hooked"}))
        );
        assert_eq!(result.permission_updates.len(), 1);
        assert!(matches!(
            result.permission_request_result,
            Some(PermissionRequestResult::Allow { .. })
        ));
    }

    #[test]
    fn process_hook_json_output_declining_elicitation_sets_official_blocking_error() {
        let parsed = parse_hook_output(
            r#"{
                "reason": "policy",
                "hookSpecificOutput": {
                    "hookEventName": "ElicitationResult",
                    "action": "decline"
                }
            }"#,
        );
        let ParsedHookOutput::Json(json) = parsed else {
            panic!("expected hook json");
        };
        let result = process_hook_json_output(&json, "echo hook");
        assert_eq!(
            result.elicitation_result_response,
            Some(HookElicitationResponse {
                action: "decline".to_string(),
                content: None,
            })
        );
        assert_eq!(result.outcome, HookOutcome::Success);
        assert_eq!(
            result
                .blocking_error
                .as_ref()
                .map(|error| error.blocking_error.as_str()),
            Some("policy")
        );
    }
}
