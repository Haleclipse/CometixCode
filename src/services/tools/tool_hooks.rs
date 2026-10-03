//! Pre/post tool-use hook integration for the tool execution flow.
//!
//! Maps to: CC `services/tools/toolHooks.ts` — PreToolUse, PostToolUse and
//! PostToolUseFailure. The PermissionRequest consumer below maps to
//! `hooks/toolPermission/PermissionContext.ts#runHooks`; PermissionDenied maps
//! to the inline loop in `services/tools/toolExecution.ts:1081`.
//!
//! The lower-level hook command execution pipeline lives in
//! `crate::services::hooks` (pre_tool/post_tool/permission_request); this module
//! owns the tool-execution-facing seam and returns the update shape consumed by
//! `tool_execution.rs`.

use crate::services::hooks::{HookContext, RegisteredHooks};
use crate::types::message::{
    AttachmentMessage, Message, RenderableMessage, RenderableMessageKind, StopHookInfo,
    SystemMessage,
};
use crate::types::permissions::{
    PermissionDecisionReason, PermissionPromptChoice, PermissionRequest, PermissionUpdate,
};

#[derive(Clone, Debug, Default)]
pub struct ToolHookSeamResult {
    pub messages: Vec<RenderableMessage>,
    /// Model-visible hook attachment messages, kept separate from UI summaries
    /// so execution can preserve their source ordering.
    pub model_messages: Vec<Message>,
    pub prevent_continuation: bool,
    /// Last `stopReason` yielded with a PreToolUse `preventContinuation` item.
    pub stop_reason: Option<String>,
    /// Maps to the distinct `stop` generator item emitted when hook execution
    /// itself is cancelled. This is not the same as `continue: false`, which
    /// allows an approved tool to run before stopping the query continuation.
    pub stop_execution: bool,
    /// Maps to CC `runPreToolUseHooks(...)` `hookUpdatedInput` stream item.
    pub updated_input: Option<serde_json::Value>,
    /// Maps to CC `runPreToolUseHooks(...)` `hookPermissionResult` behavior.
    pub permission_behavior: Option<crate::services::hooks::PermissionBehavior>,
    /// CC toolHooks.ts:486-556: message and reason belong to the last yielded
    /// hookPermissionResult, alongside behavior and updatedInput above.
    pub permission_message: Option<String>,
    pub permission_decision_reason: Option<PermissionDecisionReason>,
    /// Carries `updatedPermissions` emitted by PermissionRequest hooks.
    pub permission_updates: Vec<PermissionUpdate>,
    /// Maps to CC PermissionContext.ts:230-262: the first nested decision,
    /// including the deny message that is distinct from a blocking hook error.
    pub permission_request_result: Option<crate::types::hooks::PermissionRequestResult>,
}

/// Maps to: CC `services/tools/toolHooks.ts` `runPreToolUseHooks(...)`.
///
/// `hook_context` carries CC's `(permissionMode, toolUseContext)` pair — the
/// two arguments every `run*Hooks` function in `toolHooks.ts` hands to
/// `createBaseHookInput` through its `execute*Hooks` call
/// (`:464-471` here, `:52-53`, `:209-210` for the post pair). `base_env` is the
/// flattened env rail built from that same value; before this parameter existed
/// only the env half crossed the seam, so the JSON payload could carry neither
/// `agent_id`/`agent_type` nor `permission_mode`.
pub async fn run_pre_tool_use_hooks(
    request: &PermissionRequest,
    config: &RegisteredHooks,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> ToolHookSeamResult {
    let results = crate::services::hooks::pre_tool::execute_pre_tool_hooks(
        config,
        &request.tool_name,
        &request.tool_use_id,
        &request.input,
        hook_context,
        base_env,
        abort_controller,
    )
    .await;
    let hook_name = format!("PreToolUse:{}", request.tool_name);
    let mut output = ToolHookSeamResult::default();
    let mut processed_input_update = None;
    let mut permission_updated_input = None;
    for result in &results {
        if result.outcome == crate::services::hooks::HookOutcome::Cancelled {
            output.stop_execution = true;
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_cancelled",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PreToolUse",
                    }),
                )));
        }
        if let Some(blocking_error) = result.blocking_error.as_ref() {
            // CC toolHooks.ts:481-498, hooks.ts:1882-1887. A later
            // permissionBehavior yield below can replace this whole result.
            let message = crate::services::hooks::get_pre_tool_hook_blocking_message(
                &hook_name,
                blocking_error,
            );
            output.permission_behavior = Some(crate::services::hooks::PermissionBehavior::Deny);
            output.permission_message = Some(message.clone());
            output.permission_decision_reason = Some(PermissionDecisionReason::Hook {
                hook_name: hook_name.clone(),
                hook_source: None,
                reason: Some(message),
            });
            permission_updated_input = None;
        }
        if result.prevent_continuation {
            output.prevent_continuation = true;
            if let Some(stop_reason) = result.stop_reason.as_ref() {
                output.stop_reason = Some(stop_reason.clone());
            }
        }
        match result.permission_behavior {
            Some(permission_behavior)
                if permission_behavior
                    != crate::services::hooks::PermissionBehavior::Passthrough =>
            {
                // `toolExecution.ts` overwrites hookPermissionResult as the
                // async-generator yields each result; the final decision wins.
                output.permission_behavior = Some(permission_behavior);
                // executeHooks aggregates behavior, but each completion supplies
                // its own reason/source (hooks.ts:2862-2866). Do not retain an
                // earlier deny's reason when a later allow yields aggregated deny.
                output.permission_decision_reason = Some(PermissionDecisionReason::Hook {
                    hook_name: hook_name.clone(),
                    hook_source: result.hook_source.clone(),
                    reason: result.hook_permission_decision_reason.clone(),
                });
                output.permission_message = match permission_behavior {
                    crate::services::hooks::PermissionBehavior::Allow => None,
                    behavior => Some(result.hook_permission_decision_reason.clone()
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or_else(|| format!("Hook {hook_name} {} this tool",
                            crate::utils::permissions::permission_result::get_rule_behavior_description(
                                if behavior == crate::services::hooks::PermissionBehavior::Deny {
                                    "deny"
                                } else { "ask" }
                            )))),
                };
                permission_updated_input = if matches!(
                    permission_behavior,
                    crate::services::hooks::PermissionBehavior::Allow
                        | crate::services::hooks::PermissionBehavior::Ask
                ) {
                    result.updated_input.clone()
                } else {
                    None
                };
            }
            _ => {
                if let Some(updated_input) = result.updated_input.as_ref() {
                    // hookUpdatedInput mutates processedInput independently of
                    // hookPermissionResult in official toolExecution.ts.
                    processed_input_update = Some(updated_input.clone());
                }
            }
        }
        output
            .permission_updates
            .extend(result.permission_updates.iter().cloned());
        if let Some(additional_context) = result.additional_context.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_additional_context",
                        "content": [additional_context],
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PreToolUse",
                    }),
                )));
        }
    }
    output.updated_input = match output.permission_behavior {
        Some(crate::services::hooks::PermissionBehavior::Deny) => None,
        Some(crate::services::hooks::PermissionBehavior::Allow)
        | Some(crate::services::hooks::PermissionBehavior::Ask) => {
            permission_updated_input.or(processed_input_update)
        }
        _ => processed_input_update,
    };
    output.messages = render_tool_hook_results(
        "pre-tool-use",
        "PreToolUse",
        &results,
        output.prevent_continuation,
    );
    output
}

/// Maps to: CC `services/tools/toolHooks.ts#runPostToolUseHooks`.
pub async fn run_post_tool_use_hooks(
    request: &PermissionRequest,
    message: Option<&RenderableMessage>,
    raw_tool_response: Option<&serde_json::Value>,
    config: &RegisteredHooks,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> ToolHookSeamResult {
    let response = raw_tool_response
        .cloned()
        .unwrap_or_else(|| tool_response_for_hook(message, &request.tool_name));
    let results = crate::services::hooks::tool::execute_post_tool_hooks(
        config,
        &request.tool_name,
        &request.tool_use_id,
        &request.input,
        &response,
        hook_context,
        base_env,
        abort_controller,
    )
    .await;
    let hook_name = format!("PostToolUse:{}", request.tool_name);
    let mut output = ToolHookSeamResult::default();
    for result in &results {
        let cancelled = result.outcome == crate::services::hooks::HookOutcome::Cancelled;
        if cancelled {
            output.prevent_continuation = true;
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_cancelled",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUse",
                    }),
                )));
        }
        if let Some(blocking_error) = result.blocking_error.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_blocking_error",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUse",
                        // CC toolHooks.ts:105-115: `blockingError` carries the
                        // whole HookBlockingError object, not a bare string.
                        "blockingError": blocking_error,
                    }),
                )));
            output.prevent_continuation = true;
        }
        if result.prevent_continuation && !cancelled {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_stopped_continuation",
                        "message": result.stop_reason.clone().unwrap_or_else(||
                            "Execution stopped by PostToolUse hook".to_string()),
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUse",
                    }),
                )));
            output.prevent_continuation = true;
        }
        if let Some(additional_context) = result.additional_context.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_additional_context",
                        "content": [additional_context],
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUse",
                    }),
                )));
        }
    }
    output.messages = render_tool_hook_results(
        "post-tool-use",
        "PostToolUse",
        &results,
        output.prevent_continuation,
    );
    output
}

/// Maps to: CC `services/tools/toolHooks.ts:193-221#runPostToolUseFailureHooks`.
///
/// `is_interrupt` is CC's own parameter (`:200`), typed `boolean | undefined`
/// and forwarded verbatim to `executePostToolUseFailureHooks` (`:218`). CC's
/// only caller — `toolExecution.ts:1700-1711` — always passes a boolean, so
/// this port takes a plain `bool` and always puts the key on the wire; the
/// `Option` seam lives one level down, in the builder that mirrors the schema.
pub async fn run_post_tool_use_failure_hooks(
    request: &PermissionRequest,
    message: Option<&RenderableMessage>,
    is_interrupt: bool,
    config: &RegisteredHooks,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> ToolHookSeamResult {
    let error = tool_result_content(message).unwrap_or_else(|| "Tool execution failed".to_string());
    let results = crate::services::hooks::tool::execute_post_tool_use_failure_hooks(
        config,
        &request.tool_name,
        &request.tool_use_id,
        &request.input,
        &error,
        Some(is_interrupt),
        hook_context,
        base_env,
        abort_controller,
    )
    .await;
    let hook_name = format!("PostToolUseFailure:{}", request.tool_name);
    let mut output = ToolHookSeamResult::default();
    for result in &results {
        let cancelled = result.outcome == crate::services::hooks::HookOutcome::Cancelled;
        if cancelled {
            output.prevent_continuation = true;
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_cancelled",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUseFailure",
                    }),
                )));
        }
        if let Some(blocking_error) = result.blocking_error.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_blocking_error",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUseFailure",
                        // CC toolHooks.ts:257-267: whole HookBlockingError object.
                        "blockingError": blocking_error,
                    }),
                )));
            output.prevent_continuation = true;
        }
        if result.prevent_continuation && !cancelled {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_stopped_continuation",
                        "message": result.stop_reason.clone().unwrap_or_else(||
                            "Execution stopped by PostToolUseFailure hook".to_string()),
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUseFailure",
                    }),
                )));
            output.prevent_continuation = true;
        }
        if let Some(additional_context) = result.additional_context.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_additional_context",
                        "content": [additional_context],
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PostToolUseFailure",
                    }),
                )));
        }
    }
    output.messages = render_tool_hook_results(
        "post-tool-use-failure",
        "PostToolUseFailure",
        &results,
        output.prevent_continuation,
    );
    output
}

/// Maps to: CC `services/tools/toolExecution.ts:1081` PermissionDenied loop.
/// Rust execution-seam extraction; no same-named upstream helper exists.
pub async fn run_permission_denied_hooks_with_config(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    config: &RegisteredHooks,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> ToolHookSeamResult {
    let reason = match choice {
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow => "allowed",
        PermissionPromptChoice::Deny => "Permission denied",
    };
    let results = crate::services::hooks::tool::execute_permission_denied_hooks(
        config,
        &request.tool_name,
        &request.tool_use_id,
        &request.input,
        reason,
        hook_context,
        base_env,
        abort_controller,
    )
    .await;
    let hook_name = format!("PermissionDenied:{}", request.tool_name);
    let mut output = ToolHookSeamResult::default();
    for result in &results {
        let cancelled = result.outcome == crate::services::hooks::HookOutcome::Cancelled;
        if cancelled {
            output.prevent_continuation = true;
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_cancelled",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PermissionDenied",
                    }),
                )));
        }
        if let Some(blocking_error) = result.blocking_error.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_blocking_error",
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PermissionDenied",
                        // CC hooks.ts:712-719: whole HookBlockingError object.
                        "blockingError": blocking_error,
                    }),
                )));
            output.prevent_continuation = true;
        }
        if result.prevent_continuation && !cancelled {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_stopped_continuation",
                        "message": result.stop_reason.clone().unwrap_or_else(||
                            "Execution stopped by PermissionDenied hook".to_string()),
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PermissionDenied",
                    }),
                )));
            output.prevent_continuation = true;
        }
        if let Some(additional_context) = result.additional_context.as_ref() {
            output
                .model_messages
                .push(Message::Attachment(AttachmentMessage::new(
                    serde_json::json!({
                        "type": "hook_additional_context",
                        "content": [additional_context],
                        "hookName": hook_name,
                        "toolUseID": request.tool_use_id,
                        "hookEvent": "PermissionDenied",
                    }),
                )));
        }
    }
    output.messages = render_tool_hook_results(
        "permission-denied",
        "PermissionDenied",
        &results,
        output.prevent_continuation,
    );
    output
}

/// Maps to: CC `hooks/toolPermission/PermissionContext.ts:216-263#runHooks`.
/// Rust execution-seam extraction; `executePermissionRequestHooks` itself is
/// owned by services/hooks/permission_request.rs, not this consumer.
pub async fn run_permission_request_hooks_with_config(
    request: &PermissionRequest,
    config: &RegisteredHooks,
    hook_context: &HookContext,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> ToolHookSeamResult {
    let results =
        crate::services::hooks::permission_request::execute_permission_request_hooks_with_context(
            config,
            request,
            hook_context,
            base_env,
            abort_controller,
        )
        .await;
    let mut output = ToolHookSeamResult::default();
    // CC PermissionContext.ts:230-262 consumes the first actual nested
    // PermissionRequest decision. A top-level permissionBehavior is not one,
    // and later hook decisions must not overwrite its input or permissions.
    // executeHooks still materializes its completions here (partial latency).
    if let Some(decision) = results
        .iter()
        .find_map(|result| result.permission_request_result.as_ref())
    {
        output.permission_request_result = Some(decision.clone());
        match decision {
            crate::types::hooks::PermissionRequestResult::Allow {
                updated_input,
                updated_permissions,
            } => {
                output.permission_behavior =
                    Some(crate::services::hooks::PermissionBehavior::Allow);
                output.updated_input = updated_input.clone();
                output.permission_updates = updated_permissions.clone();
            }
            crate::types::hooks::PermissionRequestResult::Deny { interrupt, .. } => {
                output.permission_behavior = Some(crate::services::hooks::PermissionBehavior::Deny);
                if *interrupt == Some(true) {
                    if let Some(abort) = abort_controller {
                        abort.abort();
                    }
                }
            }
        }
    }
    output.messages =
        render_tool_hook_results("permission-request", "PermissionRequest", &results, false);
    output
}

/// Rust/iocraft representation adapter for CC hook progress/summary rows.
/// Domain effects (permission, input, model attachments, continuation) stay
/// in the source-named `run_*_hooks` functions above.
fn render_tool_hook_results(
    prefix: &str,
    event: &str,
    results: &[crate::services::hooks::HookResult],
    prevented_continuation: bool,
) -> Vec<RenderableMessage> {
    let mut messages = Vec::new();
    let mut hook_infos = Vec::new();
    let mut hook_errors = Vec::new();
    let mut has_output = false;
    let mut total_duration_ms = 0u64;

    for (index, result) in results.iter().enumerate() {
        if result.command.is_some() {
            hook_infos.push(StopHookInfo {
                command: result.command.clone(),
                prompt_text: None,
                duration_ms: result.duration_ms,
                output: result
                    .stdout
                    .as_ref()
                    .filter(|stdout| !stdout.trim().is_empty())
                    .cloned(),
                error: result
                    .stderr
                    .as_ref()
                    .filter(|stderr| !stderr.trim().is_empty())
                    .cloned(),
                prevented_continuation: result.prevent_continuation,
            });
        }
        total_duration_ms =
            total_duration_ms.saturating_add(result.duration_ms.unwrap_or_default());
        has_output |= result
            .stdout
            .as_deref()
            .is_some_and(|stdout| !stdout.trim().is_empty())
            || result
                .stderr
                .as_deref()
                .is_some_and(|stderr| !stderr.trim().is_empty())
            || result.outcome == crate::services::hooks::HookOutcome::Cancelled;
        if result.outcome == crate::services::hooks::HookOutcome::NonBlockingError {
            if let Some(stderr) = result
                .stderr
                .as_ref()
                .filter(|stderr| !stderr.trim().is_empty())
            {
                hook_errors.push(stderr.clone());
            }
        }
        if let Some(message) = result.system_message.clone() {
            has_output = true;
            messages.push(RenderableMessage::system(
                format!("{prefix}-message-{index}"),
                message,
            ));
        }
        if let Some(blocking_error) = result.blocking_error.as_ref() {
            hook_errors.push(blocking_error.blocking_error.clone());
            messages.push(RenderableMessage::system(
                format!("{prefix}-blocking-{index}"),
                blocking_error.blocking_error.clone(),
            ));
            has_output = true;
        }
    }

    if !hook_infos.is_empty() {
        // Maps to: CC `createStopHookSummaryMessage(...)`
        // (utils/messages.ts:4398-4426) with the tool-hook event label.
        let uuid = format!("{prefix}-summary");
        messages.push(RenderableMessage {
            uuid: uuid.clone(),
            kind: RenderableMessageKind::System(SystemMessage::StopHookSummary {
                base: crate::types::message::SystemBase::with_uuid(uuid),
                hook_label: Some(event.to_string()),
                hook_count: hook_infos.len(),
                hook_infos,
                hook_errors,
                prevented_continuation,
                stop_reason: None,
                has_output,
                level: crate::types::message::SystemMessageLevel::Info,
                tool_use_id: None,
                total_duration_ms: Some(total_duration_ms),
            }),
        });
    }
    messages
}

fn tool_response_for_hook(
    message: Option<&RenderableMessage>,
    tool_name: &str,
) -> serde_json::Value {
    let Some(tool_result) = message.and_then(user_tool_result_block) else {
        return serde_json::Value::Null;
    };
    let tool_use_id = (!tool_result.tool_use_id.0.is_empty())
        .then(|| serde_json::Value::String(tool_result.tool_use_id.0.clone()))
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "tool_use_id": tool_use_id,
        "tool_name": tool_name,
        "status": format!("{:?}", tool_result.derived_status()),
        "content": tool_result.content,
    })
}

/// One block per row (normalize-guaranteed): the row's block is the first
/// content block.
fn user_tool_result_block(
    message: &RenderableMessage,
) -> Option<&crate::types::message::ToolResult> {
    match &message.kind {
        RenderableMessageKind::User { message } => match message.first_content_block() {
            Some(crate::types::message::UserContent::ToolResult(tool_result)) => Some(tool_result),
            _ => None,
        },
        _ => None,
    }
}

fn tool_result_content(message: Option<&RenderableMessage>) -> Option<String> {
    message
        .and_then(user_tool_result_block)
        .map(|tool_result| tool_result.content.clone())
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Parse a settings-shaped hooks fixture and fold it into the
    /// execution-facing `RegisteredHooks` table.
    fn registered_from_value(
        value: serde_json::Value,
    ) -> Result<RegisteredHooks, serde_json::Error> {
        let config: crate::services::hooks::HooksConfig = serde_json::from_value(value)?;
        Ok(crate::services::hooks::test_support::registered_config(
            &config,
        ))
    }

    fn read_request(tool_use_id: &str) -> PermissionRequest {
        PermissionRequest {
            permission_result: None,
            id: format!("permission-{tool_use_id}"),
            tool_use_id: tool_use_id.to_string(),
            tool_name: "Read".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Read".to_string(),
            message: String::new(),
            input_summary: "/tmp/input.txt".to_string(),
            input: serde_json::json!({"file_path":"/tmp/input.txt"}),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new("Read", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        }
    }

    /// One `HookContext` standing in for CC's `(permissionMode,
    /// toolUseContext)` pair — the shape
    /// `services/tools/tool_execution.rs#tool_hook_context` builds in
    /// production.
    #[cfg(unix)]
    fn subagent_hook_context() -> HookContext {
        HookContext {
            session_id: "session-triple".to_string(),
            transcript_path: "/tmp/session-triple.jsonl".to_string(),
            cwd: "/tmp/triple".to_string(),
            permission_mode: Some("acceptEdits".to_string()),
            agent_id: Some("agent-triple".to_string()),
            agent_type: Some("code-reviewer".to_string()),
        }
    }

    /// Maps to: CC `utils/hooks.ts:324-326` — `permission_mode`, `agent_id` and
    /// `agent_type` are BASE keys, spread by `createBaseHookInput` into every
    /// tool-event payload (`:3419`, `:3461`, `:3510`, `:3546`, `:4175`).
    ///
    /// Old shape: `tool_event_base_input()` took no argument and started from
    /// `HookContext::default()`, so all five events shipped without the triple
    /// (PermissionRequest reconstructed `permission_mode` alone). The
    /// assertions below reported `Null` for each missing key.
    ///
    /// This drives the five `run_*` seams — the layer `tool_execution.rs`
    /// actually calls — rather than the builders, so a future seam that forgets
    /// to forward its context fails here too.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_tool_event_payload_carries_the_tool_use_context_triple() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let context = subagent_hook_context();
        let request = read_request("toolu_triple");

        async fn captured(
            event: &str,
            run: impl std::future::Future<Output = ToolHookSeamResult>,
        ) -> serde_json::Value {
            run.await;
            let path = std::env::temp_dir().join(format!("cometix-triple-{event}.json"));
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{event} hook received no stdin: {error}"));
            let _ = std::fs::remove_file(&path);
            serde_json::from_str(&raw).expect("the hook input is JSON")
        }

        fn config(event: &str) -> RegisteredHooks {
            registered_from_value(serde_json::json!({
                event: [{
                    "matcher": "Read",
                    "hooks": [{
                        "command": format!(
                            "cat > '{}'",
                            std::env::temp_dir()
                                .join(format!("cometix-triple-{event}.json"))
                                .display()
                        ),
                        "timeout": 10
                    }]
                }]
            }))
            .expect("the capture fixture parses")
        }

        let payloads = vec![
            (
                "PreToolUse",
                captured(
                    "PreToolUse",
                    run_pre_tool_use_hooks(
                        &request,
                        &config("PreToolUse"),
                        &context,
                        Vec::new(),
                        None,
                    ),
                )
                .await,
            ),
            (
                "PostToolUse",
                captured(
                    "PostToolUse",
                    run_post_tool_use_hooks(
                        &request,
                        None,
                        None,
                        &config("PostToolUse"),
                        &context,
                        Vec::new(),
                        None,
                    ),
                )
                .await,
            ),
            (
                "PostToolUseFailure",
                captured(
                    "PostToolUseFailure",
                    run_post_tool_use_failure_hooks(
                        &request,
                        None,
                        false,
                        &config("PostToolUseFailure"),
                        &context,
                        Vec::new(),
                        None,
                    ),
                )
                .await,
            ),
            (
                "PermissionDenied",
                captured(
                    "PermissionDenied",
                    run_permission_denied_hooks_with_config(
                        &request,
                        PermissionPromptChoice::Deny,
                        &config("PermissionDenied"),
                        &context,
                        Vec::new(),
                        None,
                    ),
                )
                .await,
            ),
            (
                "PermissionRequest",
                captured(
                    "PermissionRequest",
                    run_permission_request_hooks_with_config(
                        &request,
                        &config("PermissionRequest"),
                        &context,
                        Vec::new(),
                        None,
                    ),
                )
                .await,
            ),
        ];

        for (event, payload) in payloads {
            assert_eq!(payload["hook_event_name"], event);
            assert_eq!(payload["agent_id"], "agent-triple", "{event} agent_id");
            assert_eq!(payload["agent_type"], "code-reviewer", "{event} agent_type");
            assert_eq!(
                payload["permission_mode"], "acceptEdits",
                "{event} permission_mode"
            );
            // The three base keys 3933d3e landed stay put.
            assert_eq!(
                payload["session_id"], "session-triple",
                "{event} session_id"
            );
            assert_eq!(
                payload["transcript_path"], "/tmp/session-triple.jsonl",
                "{event} transcript_path"
            );
            assert_eq!(payload["cwd"], "/tmp/triple", "{event} cwd");
        }
    }

    /// Maps to: CC `services/tools/toolExecution.ts:1694`/`:1707` →
    /// `toolHooks.ts:200`/`:218` → `utils/hooks.ts:3516` — `is_interrupt` is
    /// written unconditionally, so `false` IS on the wire and only the
    /// `undefined` parameter drops the key.
    ///
    /// Old shape: `run_post_tool_use_failure_hooks` had no `is_interrupt`
    /// parameter and the builder never wrote the key, so both halves below
    /// reported `Null`.
    #[cfg(unix)]
    #[tokio::test]
    async fn post_tool_use_failure_payload_carries_is_interrupt_both_ways() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let request = read_request("toolu_interrupt");

        async fn captured(is_interrupt: bool, request: &PermissionRequest) -> serde_json::Value {
            let path = std::env::temp_dir().join(format!(
                "cometix-is-interrupt-{}.json",
                uuid::Uuid::new_v4().simple()
            ));
            let config = registered_from_value(serde_json::json!({
                "PostToolUseFailure": [{
                    "matcher": "Read",
                    "hooks": [{"command": format!("cat > '{}'", path.display()), "timeout": 10}]
                }]
            }))
            .expect("the capture fixture parses");
            run_post_tool_use_failure_hooks(
                request,
                None,
                is_interrupt,
                &config,
                &HookContext::default(),
                Vec::new(),
                None,
            )
            .await;
            let raw = std::fs::read_to_string(&path).expect("the hook received its stdin");
            let _ = std::fs::remove_file(&path);
            serde_json::from_str(&raw).expect("the hook input is JSON")
        }

        let interrupted = captured(true, &request).await;
        assert_eq!(interrupted["is_interrupt"], serde_json::json!(true));
        // The rest of the failure payload is unchanged by the flag.
        assert_eq!(interrupted["hook_event_name"], "PostToolUseFailure");
        assert_eq!(interrupted["tool_use_id"], "toolu_interrupt");
        assert_eq!(interrupted["error"], "Tool execution failed");

        let failed = captured(false, &request).await;
        assert_eq!(
            failed["is_interrupt"],
            serde_json::json!(false),
            "CC writes the key for a plain failure too — `false !== undefined`"
        );
    }

    #[tokio::test]
    async fn unconfigured_permission_denied_hooks_are_a_noop_through_canonical_path() {
        let result = run_permission_denied_hooks_with_config(
            &read_request("toolu_denied_empty"),
            PermissionPromptChoice::Deny,
            &RegisteredHooks::new(),
            &HookContext::default(),
            Vec::new(),
            None,
        )
        .await;

        assert!(result.messages.is_empty());
        assert!(result.model_messages.is_empty());
        assert!(!result.prevent_continuation);
    }

    #[test]
    fn render_tool_hook_results_builds_iocraft_summary_projection() {
        let results = vec![crate::services::hooks::HookResult {
            command: Some("echo pre".to_string()),
            duration_ms: Some(7),
            stdout: Some("pre output".to_string()),
            ..Default::default()
        }];
        let messages = render_tool_hook_results("pre-tool-use", "PreToolUse", &results, false);

        assert!(messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary {
                hook_label: Some(label),
                hook_count: 1,
                hook_infos,
                has_output: true,
                total_duration_ms: Some(7),
                ..
            }) if label == "PreToolUse"
                && hook_infos.len() == 1
                && hook_infos[0].command.as_deref() == Some("echo pre")
        )));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_tool_block_and_stop_emit_canonical_model_attachments_in_order() {
        let request = read_request("toolu_read");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PostToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": "printf '%s' '{\"continue\":false,\"stopReason\":\"stop the loop\",\"decision\":\"block\",\"reason\":\"blocked after read\",\"hookSpecificOutput\":{\"hookEventName\":\"PostToolUse\",\"additionalContext\":\"post context\"}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let result = run_post_tool_use_hooks(
            &request,
            None,
            None,
            &config,
            &HookContext::default(),
            Vec::new(),
            None,
        )
        .await;

        assert_eq!(
            result
                .model_messages
                .iter()
                .filter_map(|message| match message {
                    Message::Attachment(attachment) => Some(attachment.attachment_type()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                "hook_blocking_error",
                "hook_stopped_continuation",
                "hook_additional_context",
            ]
        );
        let Message::Attachment(stopped) = &result.model_messages[1] else {
            panic!("expected stopped attachment")
        };
        assert_eq!(stopped.attachment.to_wire()["message"], "stop the loop");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_tool_permission_result_matches_official_aggregated_precedence() {
        use crate::services::hooks::PermissionBehavior;

        let request = read_request("toolu_precedence");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"updatedInput\":{\"file_path\":\"denied\"}}}'", "timeout": 5},
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{\"file_path\":\"allowed\"}}}'", "timeout": 5}
                ]
            }]
        }))
        .unwrap();
        let deny_then_allow =
            run_pre_tool_use_hooks(&request, &config, &HookContext::default(), Vec::new(), None)
                .await;
        assert_eq!(
            deny_then_allow.permission_behavior,
            Some(PermissionBehavior::Deny)
        );
        assert!(deny_then_allow.updated_input.is_none());

        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"updatedInput\":{\"file_path\":\"asked\"}}}'", "timeout": 5},
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{\"file_path\":\"rerouted\"}}}'", "timeout": 5}
                ]
            }]
        }))
        .unwrap();
        let ask_then_allow =
            run_pre_tool_use_hooks(&request, &config, &HookContext::default(), Vec::new(), None)
                .await;
        assert_eq!(
            ask_then_allow.permission_behavior,
            Some(PermissionBehavior::Ask)
        );
        assert!(matches!(
            ask_then_allow
                .updated_input
                .as_ref()
                .and_then(|input| input["file_path"].as_str()),
            Some("asked" | "rerouted")
        ));

        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{\"file_path\":\"allowed\"}}}'", "timeout": 5},
                    {"command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\"}}'", "timeout": 5}
                ]
            }]
        }))
        .unwrap();
        let allow_then_deny =
            run_pre_tool_use_hooks(&request, &config, &HookContext::default(), Vec::new(), None)
                .await;
        assert_eq!(
            allow_then_deny.permission_behavior,
            Some(PermissionBehavior::Deny)
        );
        assert!(allow_then_deny.updated_input.is_none());
    }
}
