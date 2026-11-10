//! Prompt-hook protocol types, the hook output schemas, and the
//! PermissionRequest hook decision union.
//! Maps to: CC `types/hooks.ts` (protocol types :24-48, output schemas
//! :50-177, `PermissionRequestResult` :248-259). This file OWNS everything CC
//! declares in `types/hooks.ts`; `services/hooks/*` holds `utils/hooks.ts`
//! material only (#162 ruling). The `HookJsonOutput` serde struct in
//! `services/hooks/parsing.rs` stays as the post-parse projection of
//! `hookJSONOutputSchema`'s data.
//!
//! Not yet housed here (booked, each with a file-qualified Maps-to at its
//! current home): `HookCallback`/`HookCallbackMatcher`
//! (`types/hooks.ts:211-232`) live in `schemas/hooks.rs`; `HookProgress`
//! (`types/hooks.ts:234-241`) lives in `types/message.rs`. `HookEvent` is NOT
//! `types/hooks.ts` material — CC only imports it there (:4-9) from
//! `entrypoints/agentSdkTypes.ts`.

use serde::{Deserialize, Serialize};

/// One selectable response in a prompt hook request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRequestOption {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Maps to: CC `PromptRequest`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRequest {
    /// Request id; the `prompt` key is the wire discriminator.
    pub prompt: String,
    pub message: String,
    #[serde(default)]
    pub options: Vec<PromptRequestOption>,
}

/// Maps to: CC `PromptResponse`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptResponse {
    pub prompt_response: String,
    pub selected: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_protocol_round_trips_official_wire_keys() {
        let request: PromptRequest = serde_json::from_value(serde_json::json!({
            "prompt": "request-1",
            "message": "Choose a deployment target",
            "options": [{"key": "staging", "label": "Staging"}]
        }))
        .expect("prompt request");
        assert_eq!(request.options[0].description, None);

        let response = PromptResponse {
            prompt_response: request.prompt,
            selected: request.options[0].key.clone(),
        };
        assert_eq!(
            serde_json::to_value(response).expect("prompt response"),
            serde_json::json!({
                "prompt_response": "request-1",
                "selected": "staging"
            })
        );
    }
}

/// Maps to: CC `types/hooks.ts:50-166` `syncHookResponseSchema` — the sync
/// hook JSON response shape, `hookSpecificOutput` a fifteen-arm union keyed on
/// `hookEventName`, describe copy verbatim.
pub fn sync_hook_response_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::permissions::permission_rule::permission_behavior_schema;
        use crate::utils::permissions::permission_update_schema::permission_update_zod_schema;
        use crate::utils::zod;
        use serde_json::json;
        let watch_paths = || {
            zod::array(zod::string())
                .describe("Absolute paths to watch for FileChanged hooks")
                .optional()
        };
        let elicitation_arm = |tag: &'static str| {
            zod::object(vec![
                ("hookEventName", zod::literal(json!(tag))),
                (
                    "action",
                    zod::enumeration(vec!["accept", "decline", "cancel"]).optional(),
                ),
                ("content", zod::record(zod::any()).optional()),
            ])
        };
        zod::object(vec![
            (
                "continue",
                zod::boolean()
                    .describe("Whether nyan should continue after hook (default: true)")
                    .optional(),
            ),
            (
                "suppressOutput",
                zod::boolean()
                    .describe("Hide stdout from transcript (default: false)")
                    .optional(),
            ),
            (
                "stopReason",
                zod::string()
                    .describe("Message shown when continue is false")
                    .optional(),
            ),
            (
                "decision",
                zod::enumeration(vec!["approve", "block"]).optional(),
            ),
            (
                "reason",
                zod::string()
                    .describe("Explanation for the decision")
                    .optional(),
            ),
            (
                "systemMessage",
                zod::string()
                    .describe("Warning message shown to the user")
                    .optional(),
            ),
            (
                "hookSpecificOutput",
                zod::union(vec![
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("PreToolUse"))),
                        (
                            "permissionDecision",
                            permission_behavior_schema().clone().optional(),
                        ),
                        ("permissionDecisionReason", zod::string().optional()),
                        ("updatedInput", zod::record(zod::any()).optional()),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("UserPromptSubmit"))),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("SessionStart"))),
                        ("additionalContext", zod::string().optional()),
                        ("initialUserMessage", zod::string().optional()),
                        ("watchPaths", watch_paths()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("Setup"))),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("SubagentStart"))),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("PostToolUse"))),
                        ("additionalContext", zod::string().optional()),
                        (
                            "updatedMCPToolOutput",
                            zod::any()
                                .describe("Updates the output for MCP tools")
                                .optional(),
                        ),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("PostToolUseFailure"))),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("PermissionDenied"))),
                        ("retry", zod::boolean().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("Notification"))),
                        ("additionalContext", zod::string().optional()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("PermissionRequest"))),
                        (
                            "decision",
                            zod::union(vec![
                                zod::object(vec![
                                    ("behavior", zod::literal(json!("allow"))),
                                    ("updatedInput", zod::record(zod::any()).optional()),
                                    (
                                        "updatedPermissions",
                                        zod::array(permission_update_zod_schema().clone())
                                            .optional(),
                                    ),
                                ]),
                                zod::object(vec![
                                    ("behavior", zod::literal(json!("deny"))),
                                    ("message", zod::string().optional()),
                                    ("interrupt", zod::boolean().optional()),
                                ]),
                            ]),
                        ),
                    ]),
                    elicitation_arm("Elicitation"),
                    elicitation_arm("ElicitationResult"),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("CwdChanged"))),
                        ("watchPaths", watch_paths()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("FileChanged"))),
                        ("watchPaths", watch_paths()),
                    ]),
                    zod::object(vec![
                        ("hookEventName", zod::literal(json!("WorktreeCreate"))),
                        ("worktreePath", zod::string()),
                    ]),
                ])
                .optional(),
            ),
        ])
    })
}

/// Maps to: CC `types/hooks.ts:169-177` `hookJSONOutputSchema` —
/// `z.union([asyncHookResponseSchema, syncHookResponseSchema()])`.
pub fn hook_json_output_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        use serde_json::json;
        let async_hook_response_schema = zod::object(vec![
            ("async", zod::literal(json!(true))),
            ("asyncTimeout", zod::number().optional()),
        ]);
        zod::union(vec![
            async_hook_response_schema,
            sync_hook_response_schema().clone(),
        ])
    })
}

/// Maps to: CC `types/hooks.ts:248-259` `PermissionRequestResult` — the
/// `hookSpecificOutput.decision` union a PermissionRequest hook returns,
/// assigned wholesale to `HookResult.permissionRequestResult`
/// (`utils/hooks.ts:660`). Declared in `types/hooks.ts`; `utils/hooks.ts`
/// imports it (:66-75), so it lives here and `services/hooks` reaches over
/// (#162 — moved from `services/hooks/mod.rs`, where the bare
/// "Maps to: CC HookResult type" era had left it invisible).
///
/// The port previously kept this as a raw `serde_json::Value` with no readers,
/// so the two fields that live ONLY here — the deny arm's `message` and
/// `interrupt` — could not reach `permissions.ts:445-458` /
/// `PermissionContext.ts:245-257`. `HookResult.updated_input` and
/// `HookResult.permission_updates` stay as the port's flattened projections of
/// the allow arm (CC flattens `updatedInput` the same way at `:667-670`;
/// `permission_updates` is a port-side convenience its two consumers already
/// read), and are derived from this value at the single parse site.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionRequestResult {
    /// CC `{ behavior: 'allow', updatedInput?, updatedPermissions? }`.
    Allow {
        updated_input: Option<serde_json::Value>,
        updated_permissions: Vec<crate::types::permissions::PermissionUpdate>,
    },
    /// CC `{ behavior: 'deny', message?, interrupt? }`.
    Deny {
        message: Option<String>,
        interrupt: Option<bool>,
    },
}

impl PermissionRequestResult {
    /// Project the validated `decision` object. `None` for a behavior outside
    /// the union — unreachable through `hook_json_output_schema`, but callback
    /// hooks (`services/hooks/exec.rs#exec_callback_hook`) are validated on the
    /// print leg and project straight into the serde struct here.
    pub fn from_decision_json(decision: &serde_json::Value) -> Option<Self> {
        match decision.get("behavior").and_then(serde_json::Value::as_str) {
            Some("allow") => Some(Self::Allow {
                // CC truthy-tests this (`hooks.ts:667-670`) and nullish-tests it
                // again at `permissions.ts:423`; an explicit `null` is absent on
                // both. `z.record` already rejects it for command hooks, but
                // callback hooks project straight into the serde struct.
                updated_input: decision
                    .get("updatedInput")
                    .filter(|value| !value.is_null())
                    .cloned(),
                updated_permissions: decision
                    .get("updatedPermissions")
                    .map(crate::utils::permissions::permission_update_schema::permission_updates_from_official_json)
                    .unwrap_or_default(),
            }),
            Some("deny") => Some(Self::Deny {
                message: decision
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                interrupt: decision.get("interrupt").and_then(serde_json::Value::as_bool),
            }),
            _ => None,
        }
    }
}
