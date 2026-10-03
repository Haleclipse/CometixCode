//! PermissionRequest hook execution.
//! Maps to: CC utils/hooks.ts `executePermissionRequestHooks(...)`.
//!
//! These hooks run at the point where the interactive permission dialog would
//! otherwise be shown. They can approve/deny the request programmatically and
//! can update the tool input for allow decisions.

use super::{HookContext, HookEvent, HookResult, RegisteredHooks};
use crate::types::permissions::PermissionRequest;

/// Context-less entry point, kept for the two rails that have no threaded
/// `HookContext` yet: the SDK permission-prompt race
/// (`cli/structured_io.rs:553`, CC `structuredIO.ts:797-806`) and the
/// headless-ask tail (`utils/permissions/permissions.rs:1314`, CC
/// `permissions.ts:409-417`).
///
/// Both CC counterparts DO hand `executePermissionRequestHooks` a real
/// `toolUseContext` plus an explicit `permissionMode`, so this is a port-side
/// gap, not a CC shape: it reconstructs the `permission_mode` half from
/// `request.mode` and still sends no `agent_id`/`agent_type`. The migration
/// those two call sites need is to pass their own `ToolUseContext` (through
/// `tool_execution.rs#tool_hook_context`) to
/// [`execute_permission_request_hooks_with_context`].
pub async fn execute_permission_request_hooks(
    config: &RegisteredHooks,
    request: &PermissionRequest,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    let hook_context = HookContext {
        permission_mode: Some(
            crate::utils::permissions::permission_mode::to_external_permission_mode(request.mode)
                .to_string(),
        ),
        ..Default::default()
    };
    execute_permission_request_hooks_with_context(
        config,
        request,
        &hook_context,
        base_env,
        abort_controller,
    )
    .await
}

/// Maps to: CC `utils/hooks.ts:4157-4192#executePermissionRequestHooks`, with
/// CC's `(permissionMode, toolUseContext)` argument pair carried by
/// `hook_context`.
///
/// `permission_suggestions` is CC's `permissionSuggestions` parameter
/// (`:4164`, `:4179`) — "Optional permission suggestions (the 'always allow'
/// options)" (`:4152`). Every CC caller hands it the suggestions the PRECEDING
/// permission evaluation produced, never a locally built one:
///
/// - `cli/structuredIO.ts:577-583` — `executePermissionRequestHooksForSDK(...,
///   mainPermissionResult.suggestions)`, forwarded verbatim at `:798-806`;
/// - `utils/permissions/permissions.ts:932-940` — the headless tail passes
///   `result.suggestions` into `runPermissionRequestHooksForHeadlessAgent`,
///   forwarded at `:412-420`;
/// - `hooks/toolPermission/PermissionContext.ts:216-229` — `runHooks`'s own
///   `suggestions` parameter.
///
/// This port carries that same value on `PermissionRequest::suggestions` (the
/// documented projection of CC `ToolUseConfirm.permissionResult.suggestions`,
/// written by `permissions.rs#permission_request_from_tool_result`), so all
/// three call sites are served by reading it off the request.
///
/// The port used to synthesize a single session-scoped `addRules` allow from
/// `request.rule` instead. That is not a value CC ever computes: a Bash
/// compound command's per-subcommand suggestions collapsed to one rule, a
/// `setMode`/`addDirectories` suggestion became an `addRules`, and a tool that
/// suggested nothing still handed hooks a fabricated "always allow" option.
///
/// CC's schema types the key `z.array(PermissionUpdateSchema()).optional()`
/// (`entrypoints/sdk/coreSchemas.ts:425-433`) and `JSON.stringify` drops an
/// `undefined` value, so an absent suggestion list omits the key. `Vec` cannot
/// distinguish `[]` from `undefined`, so both omit — the same flattening
/// `cli/print.rs#can_use_tool_control_request` already documents for the
/// `can_use_tool` payload built from this very field.
pub async fn execute_permission_request_hooks_with_context(
    config: &RegisteredHooks,
    request: &PermissionRequest,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    crate::utils::debug::log_for_debugging(&format!(
        "executePermissionRequestHooks called for tool: {}",
        request.tool_name
    ));
    // Maps to: CC `hooks.ts:4174-4180`. `PermissionRequestHookInputSchema`
    // (`entrypoints/sdk/coreSchemas.ts:425-434`) is the base plus exactly
    // `hook_event_name`, `tool_name`, `tool_input`, `permission_suggestions?`
    // — there is NO `tool_use_id`. Its sibling `PreToolUseHookInputSchema`
    // (`:414-423`) does declare one, and `hooks.ts:3418-3424` writes it only
    // there; the port had copied that key onto this event, so a hook script
    // reading `tool_use_id` saw a field CC never sends for PermissionRequest.
    //
    // `permission_mode` is a BASE key here, not an event key: CC passes it as
    // `createBaseHookInput(permissionMode, …)` (`:4175`), and so are
    // `agent_id`/`agent_type`, which ride the same `toolUseContext` argument.
    let mut hook_input = super::tool::tool_event_base_input(hook_context)
        .set("hook_event_name", "PermissionRequest")
        .set("tool_name", request.tool_name.as_str())
        .set("tool_input", request.input.clone())
        .build();
    if !request.suggestions.is_empty() {
        hook_input["permission_suggestions"] = serde_json::Value::Array(
            crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
                &request.suggestions,
            ),
        );
    }

    super::tool::execute_hooks(
        config,
        HookEvent::PermissionRequest,
        &request.tool_name,
        &request.tool_use_id,
        &request.input,
        &hook_input,
        &base_env,
        abort_controller,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::{HookCommand, HookConfigEntry, HookOutcome};
    use crate::types::permissions::{
        PermissionBehavior as RulePermissionBehavior, PermissionMode, PermissionRuleValue,
        PermissionUpdate, PermissionUpdateDestination,
    };
    use crate::utils::permissions::permissions::mock_permission_request_with_input;
    use std::collections::HashMap;

    fn bash_request() -> crate::types::permissions::PermissionRequest {
        mock_permission_request_with_input(
            "perm-bash".to_string(),
            "toolu_bash".to_string(),
            "Bash".to_string(),
            "echo original".to_string(),
            serde_json::json!({"command": "echo original"}),
            PermissionMode::Default,
        )
    }

    fn hook_config(command: &str) -> crate::services::hooks::RegisteredHooks {
        let mut config = HashMap::new();
        config.insert(
            "PermissionRequest".to_string(),
            vec![HookConfigEntry {
                matcher: Some("Bash".to_string()),
                hooks: vec![HookCommand {
                    command: command.to_string(),
                    shell: None,
                    timeout: Some(5),
                    condition: None,
                    status: None,
                    once: None,
                    is_async: None,
                    async_rewake: None,
                }],
                plugin_root: None,
                plugin_name: None,
                plugin_id: None,
            }],
        );
        crate::services::hooks::test_support::registered_config(&config)
    }

    /// Run the hooks with a command that dumps its stdin to `$CAPTURE`, and
    /// return the parsed hook input. `base_env` is the callers' env channel
    /// into `exec_command_hook`, so the capture path rides the real plumbing.
    async fn captured_hook_input(
        request: &crate::types::permissions::PermissionRequest,
    ) -> serde_json::Value {
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let capture = std::env::temp_dir().join(format!(
            "cometix-permission-request-hook-input-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let config = hook_config(r#"cat > "$COMETIX_TEST_HOOK_INPUT_CAPTURE""#);
        execute_permission_request_hooks(
            &config,
            request,
            vec![(
                "COMETIX_TEST_HOOK_INPUT_CAPTURE".to_string(),
                capture.display().to_string(),
            )],
            None,
        )
        .await;
        let raw = std::fs::read_to_string(&capture).expect("the hook received its stdin");
        let _ = std::fs::remove_file(&capture);
        serde_json::from_str(&raw).expect("the hook input is JSON")
    }

    #[tokio::test]
    async fn permission_request_valid_json_exit_two_matches_official_updated_input() {
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let config = hook_config(
            r#"input=$(cat); printf '%s' "$input" | grep -q '"hook_event_name":"PermissionRequest"' || exit 2; printf '%s' "$input" | grep -q '"permission_mode":"default"' || exit 2; printf '%s' "$input" | grep -q '"type":"addRules"' || exit 2; printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedInput":{"command":"echo hooked"}}}}'; exit 2"#,
        );
        let mut request = bash_request();
        // The `"type":"addRules"` assertion above used to pass on a request
        // with NO suggestions, because the hook input fabricated one. It now
        // has to come from the evaluation.
        request.suggestions = vec![PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::Session,
            behavior: RulePermissionBehavior::Allow,
            rules: vec![PermissionRuleValue::new(
                "Bash",
                Some("echo original".to_string()),
            )],
        }];

        let results = execute_permission_request_hooks(&config, &request, vec![], None).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, HookOutcome::Success);
        assert_eq!(
            results[0].permission_behavior,
            Some(super::super::PermissionBehavior::Allow)
        );
        assert_eq!(
            results[0]
                .updated_input
                .as_ref()
                .and_then(|v| v.get("command")),
            Some(&serde_json::json!("echo hooked"))
        );
    }

    /// CC hands `executePermissionRequestHooks` the suggestions the preceding
    /// permission evaluation produced — `mainPermissionResult.suggestions`
    /// (`cli/structuredIO.ts:577-583`), `result.suggestions`
    /// (`utils/permissions/permissions.ts:932-940`), `runHooks(…, suggestions,
    /// …)` (`hooks/toolPermission/PermissionContext.ts:216-229`) — and puts
    /// that array on the wire unchanged (`utils/hooks.ts:4179`).
    ///
    /// Old shape (verified by restoring the fabrication, 2026-08-29): the input
    /// carried exactly one entry —
    /// `{addRules, session, allow, rules:[Bash(echo original)]}` built from
    /// `request.rule`. The two `projectSettings` per-subcommand rules and the
    /// whole `setMode` update were gone, and the destination was wrong; the
    /// equality assertion below reported `left` of length 1 against `right` of
    /// length 2.
    #[tokio::test]
    async fn permission_request_hook_input_carries_the_real_evaluation_suggestions() {
        let mut request = bash_request();
        request.suggestions = vec![
            PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::ProjectSettings,
                behavior: RulePermissionBehavior::Allow,
                rules: vec![
                    PermissionRuleValue::new("Bash", Some("git status".to_string())),
                    PermissionRuleValue::new("Bash", Some("git diff".to_string())),
                ],
            },
            PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            },
        ];
        // The rule is deliberately unrelated to the suggestions: it is what the
        // fabricated entry used to be built from.
        request.rule = PermissionRuleValue::new("Bash", Some("echo original".to_string()));

        let hook_input = captured_hook_input(&request).await;
        assert_eq!(
            hook_input["permission_suggestions"],
            serde_json::Value::Array(
                crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
                    &request.suggestions,
                ),
            ),
            "the hook input must carry the evaluation's own suggestions verbatim"
        );
        let suggestions = hook_input["permission_suggestions"]
            .as_array()
            .expect("an array");
        assert_eq!(suggestions.len(), 2, "both updates survive, not just one");
        assert_eq!(suggestions[0]["destination"], "projectSettings");
        assert_eq!(
            suggestions[0]["rules"].as_array().expect("an array").len(),
            2,
            "both per-subcommand rules survive"
        );
        assert_eq!(suggestions[1]["type"], "setMode");
        assert_eq!(suggestions[1]["mode"], "acceptEdits");
    }

    /// CC types the key `z.array(PermissionUpdateSchema()).optional()`
    /// (`entrypoints/sdk/coreSchemas.ts:425-433`) and `JSON.stringify` drops an
    /// `undefined` value, so a tool that suggested nothing sends no key at all.
    ///
    /// Old shape: the input always carried a fabricated single
    /// `addRules`/`allow`/`session` entry built from `request.rule` — an
    /// "always allow" option no CC evaluation had offered — so a hook keying
    /// off `permission_suggestions` saw one where CC shows none.
    #[tokio::test]
    async fn permission_request_hook_input_omits_suggestions_when_the_evaluation_had_none() {
        let request = bash_request();
        assert!(
            request.suggestions.is_empty(),
            "precondition: the evaluation produced no suggestions"
        );

        let hook_input = captured_hook_input(&request).await;
        assert!(
            hook_input.get("permission_suggestions").is_none(),
            "no suggestions means no key, not a fabricated one: {hook_input}"
        );
        // The rest of the input is unchanged by the omission.
        assert_eq!(hook_input["hook_event_name"], "PermissionRequest");
        assert_eq!(hook_input["tool_name"], "Bash");
        assert_eq!(hook_input["permission_mode"], "default");
    }
}
