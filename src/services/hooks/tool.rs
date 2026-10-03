//! Tool-event hook execution slice.
//!
//! CC defines these functions in the monolithic `utils/hooks.ts`. The user-authorized
//! Rust decomposition places focused hook owners under `services/hooks/`; this module
//! owns only the PostToolUse, PostToolUseFailure, PermissionDenied, and shared tool-event
//! `executeHooks` slice and is not the canonical counterpart for the whole upstream file.

use crate::services::hooks::exec::exec_command_hook;
use crate::services::hooks::matching::get_matching_hooks;
use crate::services::hooks::parsing::{
    ParsedHookOutput, parse_hook_output, process_hook_json_output_for_event,
};
use crate::services::hooks::{
    HookBlockingError, HookContext, HookEvent, HookOutcome, HookResult, RegisteredHooks,
};
use futures::StreamExt;
use std::time::{Duration, Instant};

const TOOL_HOOK_TIMEOUT_MS: u64 = 30_000;

/// The `...createBaseHookInput(permissionMode, undefined, toolUseContext)`
/// spread every tool-event builder opens with (CC `hooks.ts:3419`, `:3461`,
/// `:3510`, `:3546`, `:4175`).
///
/// `hook_context` is this port's carrier for CC's `(permissionMode,
/// toolUseContext)` pair; `services/tools/tool_execution.rs#tool_hook_context`
/// is the constructor that fills it. It used to be
/// `HookContext::default()` here, which meant PreToolUse / PostToolUse /
/// PostToolUseFailure / PermissionDenied / PermissionRequest could carry
/// NEITHER `agent_id`/`agent_type` (so a hook script could not tell a subagent's
/// tool call from the main thread's — CC's documented use for `agent_id`,
/// `hooks.ts:317-318`) nor `permission_mode` (except PermissionRequest, which
/// built its own).
pub(super) fn tool_event_base_input(
    hook_context: &HookContext,
) -> crate::services::hooks::HookInputBuilder {
    crate::services::hooks::HookInputBuilder::from_base(
        crate::services::hooks::create_base_hook_input_object(hook_context),
    )
}

/// Maps to: CC `utils/hooks.ts:3450-3477#executePostToolHooks`.
pub async fn execute_post_tool_hooks(
    config: &RegisteredHooks,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
    tool_response: &serde_json::Value,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    let hook_input = tool_event_base_input(hook_context)
        .set("hook_event_name", "PostToolUse")
        .set("tool_name", tool_name)
        .set("tool_input", tool_input.clone())
        .set("tool_response", tool_response.clone())
        .set("tool_use_id", tool_use_id)
        .build();
    execute_hooks(
        config,
        HookEvent::PostToolUse,
        tool_name,
        tool_use_id,
        tool_input,
        &hook_input,
        &base_env,
        abort_controller,
    )
    .await
}

/// Maps to: CC `utils/hooks.ts:3492-3527#executePostToolUseFailureHooks`.
///
/// `is_interrupt` (`coreSchemas.ts:456`, `z.boolean().optional()`) is written
/// unconditionally by CC as `is_interrupt: isInterrupt` (`hooks.ts:3516`), so
/// `JSON.stringify` drops the key only when the parameter itself is `undefined`
/// — `false` IS on the wire. `Option<bool>` reproduces that exactly: `Some`
/// emits, `None` omits.
///
/// Its single CC producer is `services/tools/toolExecution.ts:1694` — `const
/// isInterrupt = error instanceof AbortError` — inside the `catch` that calls
/// `runPostToolUseFailureHooks(…, isInterrupt, …)` (`:1707`), forwarded
/// verbatim by `toolHooks.ts:200`/`:218`. The port's counterpart is
/// `tool_execution.rs#post_tool_hook_event`.
pub async fn execute_post_tool_use_failure_hooks(
    config: &RegisteredHooks,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
    error: &str,
    is_interrupt: Option<bool>,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    let hook_input = tool_event_base_input(hook_context)
        .set("hook_event_name", "PostToolUseFailure")
        .set("tool_name", tool_name)
        .set("tool_input", tool_input.clone())
        .set("tool_use_id", tool_use_id)
        .set("error", error)
        .set_optional("is_interrupt", is_interrupt)
        .build();
    execute_hooks(
        config,
        HookEvent::PostToolUseFailure,
        tool_name,
        tool_use_id,
        tool_input,
        &hook_input,
        &base_env,
        abort_controller,
    )
    .await
}

/// Maps to: CC `utils/hooks.ts:3529-3562#executePermissionDeniedHooks`.
pub async fn execute_permission_denied_hooks(
    config: &RegisteredHooks,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
    reason: &str,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    let hook_input = tool_event_base_input(hook_context)
        .set("hook_event_name", "PermissionDenied")
        .set("tool_name", tool_name)
        .set("tool_input", tool_input.clone())
        .set("tool_use_id", tool_use_id)
        .set("reason", reason)
        .build();
    execute_hooks(
        config,
        HookEvent::PermissionDenied,
        tool_name,
        tool_use_id,
        tool_input,
        &hook_input,
        &base_env,
        abort_controller,
    )
    .await
}

/// Maps to: CC `utils/hooks.ts:1952-2972#executeHooks`, limited to the
/// current command/callback tool-event slice.
/// Deviation (L2): `Hook monolith service decomposition` keeps shared execution
/// here while source-named event modules retain their payload construction.
pub(super) async fn execute_hooks(
    config: &RegisteredHooks,
    event: HookEvent,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &serde_json::Value,
    input_json: &serde_json::Value,
    base_env: &[(String, String)],
    abort_controller: Option<&crate::tool::AbortController>,
) -> Vec<HookResult> {
    // Maps to: CC `utils/hooks.ts:1978-1999` — the gate block `executeHooks`
    // opens with, BEFORE it resolves the session id, assembles `getHooksConfig`,
    // or matches anything. Placing it here rather than in each event wrapper is
    // the point: a managed `disableAllHooks` policy and the workspace-trust
    // check are both admin/security controls, so every source that reaches this
    // executor — settings, registered/plugin, and the session-derived hooks the
    // callers merge in — has to be stopped by the same gate. `match_query` is
    // CC's, i.e. the tool name (`:3437`, `:3455`, `:3522`, `:3555`, `:4183`).
    if crate::services::hooks::should_skip_hook_execution(event, tool_name) {
        return Vec::new();
    }

    let matched = get_matching_hooks(config, event, tool_name, Some(tool_input));
    if matched.is_empty() {
        return Vec::new();
    }

    let input_str = input_json.to_string();
    // CC hooks.ts:2143,2739 starts every matched hook and observes completion
    // order. A blocking result is data, not permission to skip sibling hooks.
    // This existing Vec boundary still waits for all completions before its
    // consumers see them; PermissionRequest early-return latency remains partial.
    let mut pending = futures::stream::FuturesUnordered::new();
    for hook in &matched {
        let input_str = &input_str;
        pending.push(async move {
            // CC hooks.ts:2147 — callback hooks resolve via the SDK consumer.
            let mut result = match &hook.hook {
                crate::schemas::hooks::RegisteredHook::Callback(callback) => {
                    let start = Instant::now();
                    let json = super::exec::exec_callback_hook(
                        callback,
                        input_str,
                        Some(tool_use_id.to_string()),
                    )
                    .await;
                    // Callbacks don't have stdout/stderr/exitCode
                    // (hooks.ts:4879-4888), so command completion semantics below
                    // never apply to them.
                    let mut result =
                        process_hook_json_output_for_event(&json, "callback", event.as_str());
                    result.command = Some("callback".to_string());
                    result.duration_ms = Some(start.elapsed().as_millis() as u64);
                    result
                }
                crate::schemas::hooks::RegisteredHook::Command(command) => {
                    let start = Instant::now();
                    let timeout = Duration::from_millis(
                        command.timeout.unwrap_or(TOOL_HOOK_TIMEOUT_MS / 1000) * 1000,
                    );
                    let exec_result = exec_command_hook(
                        &command.command,
                        input_str,
                        timeout,
                        base_env.to_vec(),
                        hook.plugin_root.as_deref(),
                        hook.plugin_id.as_deref(),
                        abort_controller,
                    )
                    .await;

                    let parsed = parse_hook_output(&exec_result.stdout);
                    let parsed_json = matches!(&parsed, ParsedHookOutput::Json(_));
                    let mut result = match parsed {
                        ParsedHookOutput::Json(json) => process_hook_json_output_for_event(
                            &json,
                            &command.command,
                            event.as_str(),
                        ),
                        ParsedHookOutput::PlainText(text) => HookResult {
                            system_message: Some(text),
                            ..Default::default()
                        },
                        ParsedHookOutput::ValidationError { error, .. } => HookResult {
                            outcome: HookOutcome::NonBlockingError,
                            system_message: Some(format!("Hook output error: {error}")),
                            ..Default::default()
                        },
                        ParsedHookOutput::Empty => HookResult::default(),
                    };

                    // CC `executeHooks` applies command completion semantics only after it
                    // knows whether stdout was valid JSON. In particular, plain/empty exit
                    // 2 is blocking, valid JSON keeps its payload, and JSON validation
                    // errors remain non-blocking validation errors.
                    result.command = Some(command.command.clone());
                    result.duration_ms = Some(start.elapsed().as_millis() as u64);
                    result.stdout = Some(exec_result.stdout.clone());
                    result.stderr = Some(exec_result.stderr.clone());
                    if exec_result.aborted {
                        result.outcome = HookOutcome::Cancelled;
                        result.system_message = None;
                        result.permission_behavior = None;
                        result.updated_input = None;
                        result.prevent_continuation = true;
                    } else if !parsed_json
                        && exec_result.status == 2
                        && result.outcome == HookOutcome::Success
                    {
                        result.outcome = HookOutcome::Blocking;
                        result.system_message = None;
                        result.permission_behavior = None;
                        result.updated_input = None;
                        result.blocking_error = Some(HookBlockingError {
                            blocking_error: format!(
                                "[{}]: {}",
                                command.command,
                                if exec_result.stderr.is_empty() {
                                    "No stderr output"
                                } else {
                                    exec_result.stderr.as_str()
                                }
                            ),
                            command: command.command.clone(),
                        });
                    } else if !parsed_json
                        && exec_result.status != 0
                        && result.outcome == HookOutcome::Success
                    {
                        result.outcome = HookOutcome::NonBlockingError;
                    }
                    result
                }
            };

            result.hook_source = hook.hook_source.clone();
            result
        });
    }

    let mut results = Vec::new();
    let mut permission_behavior = None;
    while let Some(mut result) = pending.next().await {
        // Maps to CC hooks.ts:2820-2867. This is executeHooks' aggregation,
        // before toolHooks receives a yield. Ranking the raw results later at
        // the caller loses the separate updatedInput-only yield (:2872-2880).
        let raw_behavior = result.permission_behavior;
        let raw_input = result.updated_input.take();
        use super::PermissionBehavior;
        match raw_behavior {
            Some(PermissionBehavior::Deny) => permission_behavior = Some(PermissionBehavior::Deny),
            Some(PermissionBehavior::Ask)
                if permission_behavior != Some(PermissionBehavior::Deny) =>
            {
                permission_behavior = Some(PermissionBehavior::Ask);
            }
            Some(PermissionBehavior::Allow) if permission_behavior.is_none() => {
                permission_behavior = Some(PermissionBehavior::Allow);
            }
            _ => {}
        }
        result.permission_behavior = permission_behavior;
        if permission_behavior.is_some()
            && matches!(
                raw_behavior,
                Some(PermissionBehavior::Allow | PermissionBehavior::Ask)
            )
        {
            result.updated_input = raw_input.clone();
        } else if permission_behavior.is_none() && raw_behavior.is_none() {
            result.updated_input = raw_input.clone();
        }
        results.push(result);
        if permission_behavior.is_some() && raw_behavior.is_none() && raw_input.is_some() {
            // CC emits an independent hookUpdatedInput even after an earlier
            // decision. No command metadata: this is a second yield, not a
            // second execution or a second user-facing completion summary.
            results.push(HookResult {
                updated_input: raw_input,
                ..HookResult::default()
            });
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn mismatched_post_tool_event_specific_output_is_ignored() {
        let json: crate::services::hooks::parsing::HookJsonOutput =
            serde_json::from_value(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "additionalContext": "must not be injected"
                }
            }))
            .unwrap();
        let result = process_hook_json_output_for_event(&json, "bad-post", "PostToolUse");
        assert_eq!(result.outcome, HookOutcome::NonBlockingError);
        assert!(result.additional_context.is_none());
    }

    #[cfg(not(windows))]
    fn post_hook_config(command: &str) -> RegisteredHooks {
        use crate::services::hooks::{HookCommand, HookConfigEntry};
        let config: crate::services::hooks::HooksConfig = std::collections::HashMap::from([(
            HookEvent::PostToolUse.as_str().to_string(),
            vec![HookConfigEntry {
                matcher: Some("Read".to_string()),
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
        )]);
        crate::services::hooks::test_support::registered_config(&config)
    }

    /// Maps to: CC `utils/hooks.ts:1978-1980` — the FIRST thing `executeHooks`
    /// does is `if (shouldDisableAllHooksIncludingManaged()) { return }`.
    ///
    /// The claim under test is structural, not per-event: the gate sits in the
    /// shared executor, so both events routed through it stop, and so would a
    /// fourth one added tomorrow. The old shape had no gate here at all, which
    /// left the session-hook arm the callers merge in (agent frontmatter, skill
    /// hooks) as the one source an enterprise `disableAllHooks` failed to stop.
    ///
    /// Non-vacuity is asserted in-test rather than by deleting the guard: the
    /// SAME two configs run first with an empty managed root (one result each)
    /// and then with the policy installed (zero). Removing the guard turns the
    /// second half's `assert!(…is_empty())` into `1 != 0`.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn managed_disable_all_hooks_policy_stops_every_event_at_the_shared_executor() {
        use crate::services::hooks::test_support::{ManagedSettingsGuard, SessionTrustGuard};

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        // The same executor now also carries CC's trust gate (`hooks.ts:1994`),
        // so the "allowed" half has to state trust rather than inherit it from
        // the developer's `.claude.json`.
        let _trust = SessionTrustGuard::accepted();

        async fn run_both_events() -> (Vec<HookResult>, Vec<HookResult>) {
            let post = execute_post_tool_hooks(
                &post_hook_config("printf hooked"),
                "Read",
                "toolu_policy_post",
                &serde_json::json!({"file_path": "/tmp/source.txt"}),
                &serde_json::json!({"type": "text"}),
                &HookContext::default(),
                vec![],
                None,
            )
            .await;
            let denied = execute_permission_denied_hooks(
                &denied_hook_config("printf hooked"),
                "Read",
                "toolu_policy_denied",
                &serde_json::json!({"file_path": "/tmp/source.txt"}),
                "Permission denied",
                &HookContext::default(),
                vec![],
                None,
            )
            .await;
            (post, denied)
        }

        // Baseline: an EMPTY managed root, so the developer's real policy file
        // cannot make this half pass for the wrong reason.
        let allowed = {
            let _managed = ManagedSettingsGuard::install(None);
            run_both_events().await
        };
        assert_eq!(allowed.0.len(), 1, "PostToolUse runs without the policy");
        assert_eq!(
            allowed.1.len(),
            1,
            "PermissionDenied runs without the policy"
        );

        let blocked = {
            let _managed = ManagedSettingsGuard::install(Some(r#"{"disableAllHooks": true}"#));
            run_both_events().await
        };
        assert!(
            blocked.0.is_empty(),
            "PostToolUse must stop at the shared executor, got {:?}",
            blocked.0.len()
        );
        assert!(
            blocked.1.is_empty(),
            "PermissionDenied must stop at the same gate, got {:?}",
            blocked.1.len()
        );
    }

    #[cfg(not(windows))]
    fn denied_hook_config(command: &str) -> RegisteredHooks {
        use crate::services::hooks::{HookCommand, HookConfigEntry};
        let config: crate::services::hooks::HooksConfig = std::collections::HashMap::from([(
            HookEvent::PermissionDenied.as_str().to_string(),
            vec![HookConfigEntry {
                matcher: Some("Read".to_string()),
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
        )]);
        crate::services::hooks::test_support::registered_config(&config)
    }

    /// Maps to: CC `entrypoints/sdk/coreSchemas.ts:456`
    /// (`is_interrupt: z.boolean().optional()`) and `utils/hooks.ts:3516`
    /// (`is_interrupt: isInterrupt`, written unconditionally).
    ///
    /// `JSON.stringify` drops a property whose value is `undefined`, so the
    /// `None` arm has to send NO key — hook scripts branch on presence
    /// (`'is_interrupt' in data`). The `Some(false)` arm is the one that would
    /// be wrong under a naive "skip falsy" port: `false` IS on the wire.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn absent_is_interrupt_sends_no_key_at_all() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();

        let capture = crate::services::hooks::test_support::HookInputCapture::new();
        execute_post_tool_use_failure_hooks(
            &capture.config("PostToolUseFailure", Some("Read")),
            "Read",
            "toolu_no_flag",
            &serde_json::json!({"file_path": "/tmp/source.txt"}),
            "boom",
            None,
            &HookContext::default(),
            capture.base_env(),
            None,
        )
        .await;
        let payload = capture.read();
        assert_eq!(payload["error"], "boom");
        assert!(
            payload.get("is_interrupt").is_none(),
            "an undefined isInterrupt sends no key, not null: {payload}"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn valid_json_exit_two_matches_official_processed_payload() {
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let config = post_hook_config(
            r#"printf '%s\n' '{"systemMessage":"from-json"}'; printf 'ignored status' >&2; exit 2"#,
        );
        let results = execute_post_tool_hooks(
            &config,
            "Read",
            "toolu_read_json",
            &serde_json::json!({"file_path": "/tmp/source.txt"}),
            &serde_json::json!({"type": "text"}),
            &HookContext::default(),
            vec![],
            None,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, HookOutcome::Success);
        assert_eq!(results[0].system_message.as_deref(), Some("from-json"));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn non_json_post_tool_exit_two_is_blocking() {
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let command = "printf 'ignored stdout'; printf 'post policy blocked' >&2; exit 2";
        let config = post_hook_config(command);
        let results = execute_post_tool_hooks(
            &config,
            "Read",
            "toolu_read_plain",
            &serde_json::json!({"file_path": "/tmp/source.txt"}),
            &serde_json::json!({"type": "text"}),
            &HookContext::default(),
            vec![],
            None,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, HookOutcome::Blocking);
        assert!(results[0].system_message.is_none());
        let expected = format!("[{command}]: post policy blocked");
        assert_eq!(
            results[0]
                .blocking_error
                .as_ref()
                .map(|error| error.blocking_error.as_str()),
            Some(expected.as_str())
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn malformed_json_post_tool_exit_two_matches_official_plain_text_fallback() {
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let command = "printf '{'; printf 'malformed output blocked' >&2; exit 2";
        let config = post_hook_config(command);
        let results = execute_post_tool_hooks(
            &config,
            "Read",
            "toolu_read_malformed",
            &serde_json::json!({"file_path": "/tmp/source.txt"}),
            &serde_json::json!({"type": "text"}),
            &HookContext::default(),
            vec![],
            None,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, HookOutcome::Blocking);
        assert!(results[0].system_message.is_none());
        assert_eq!(
            results[0]
                .blocking_error
                .as_ref()
                .map(|error| error.blocking_error.as_str()),
            Some(format!("[{command}]: malformed output blocked").as_str())
        );
    }
}
