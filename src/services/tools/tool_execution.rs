//! Maps to: CC `services/tools/toolExecution.ts` — the orchestration layer.
//! Official flow: `runToolUse(...)` calls `canUseTool(...)` before executing a
//! tool. If the result is `ask`, the interactive permission handler pushes a
//! `ToolUseConfirm` into REPL state and the tool execution awaits the user's
//! decision. Real tool I/O runs in each `tools/<tool>` module through the
//! `crate::tool::ToolCall` trait (mirroring CC per-tool `call()`); this layer
//! owns permission flow, delegates hook seams to `tool_hooks.rs`, and handles
//! result wrapping plus registry dispatch.
//! Tools without a ported execution body still use their registered safe-stub
//! `ToolCall` implementations (not CC parity; each stub is drained per tool).

use crate::hooks::use_can_use_tool::{
    CanUseToolParams, CanUseToolResult, ToolUsePermissionRequest,
    can_use_tool_or_queue_permission_for_params_with_store,
};
use crate::services::hooks::RegisteredHooks;

use crate::services::tools::tool_hooks::{
    ToolHookSeamResult, run_permission_denied_hooks_with_config,
    run_permission_request_hooks_with_config, run_post_tool_use_failure_hooks,
    run_post_tool_use_hooks, run_pre_tool_use_hooks,
};
use crate::tool::ToolUseContext;
use crate::types::message::{
    AssistantMessage, AttachmentMessage, Message, ToolResult, ToolUseBlock, UserContent,
    UserMessage,
};
use crate::types::message::{RenderableMessage, RenderableMessageKind, ToolResultStatus};
use crate::types::permissions::{
    PermissionDecision, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest, PermissionRuleValue, PermissionUpdate, PromptDecision, ToolUseConfirm,
};
use crate::utils::permissions::permissions::{
    apply_prompt_response, has_in_memory_allow_rule, mock_permission_request_with_input,
};

/// Rust equivalent of CC `MessageUpdateLazy.contextModifier`.
/// Maps to: CC `services/tools/toolExecution.ts` `MessageUpdateLazy`.
///
/// The TypeScript field carries a closure. Rust stores the typed operation so
/// the orchestration layer can apply it deterministically after concurrent
/// batches, matching CC `runTools(...)` queuing semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolContextModifier {
    pub tool_use_id: String,
    pub operation: ToolContextModifierOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolContextModifierOperation {
    MarkComplete,
    /// Commutative permission-rule merge for concurrent batches: hook-issued
    /// `updatedPermissions` must survive out-of-order sibling completion
    /// instead of being lost when a later worker's snapshot context wins.
    ApplyPermissionUpdates(Vec<crate::types::permissions::PermissionUpdate>),
    /// Maps to: CC `tools/SkillTool/SkillTool.ts:775-839` — the only
    /// `ToolResult.contextModifier` producer in the tree.
    Skill(crate::tools::skill_tool::SkillContextModifier),
}

impl ToolContextModifier {
    pub fn mark_complete(tool_use_id: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            operation: ToolContextModifierOperation::MarkComplete,
        }
    }

    pub fn apply_permission_updates(
        tool_use_id: impl Into<String>,
        updates: Vec<crate::types::permissions::PermissionUpdate>,
    ) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            operation: ToolContextModifierOperation::ApplyPermissionUpdates(updates),
        }
    }

    /// Maps to: CC `toolExecution.ts:1465-1470` pairing the tool's
    /// `contextModifier` with the executing tool_use id before orchestration
    /// applies it.
    pub fn from_operation(
        tool_use_id: impl Into<String>,
        operation: ToolContextModifierOperation,
    ) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            operation,
        }
    }

    pub fn modify_context(&self, mut context: ToolUseContext) -> ToolUseContext {
        match &self.operation {
            ToolContextModifierOperation::MarkComplete => {
                context.mark_complete(&self.tool_use_id);
            }
            ToolContextModifierOperation::ApplyPermissionUpdates(updates) => {
                let next = crate::utils::permissions::permission_update::apply_permission_updates(
                    &context.tool_permission_context,
                    updates,
                );
                context.update_permission_context(next);
            }
            ToolContextModifierOperation::Skill(skill) => {
                skill.modify_context(&mut context);
            }
        }
        context
    }
}

/// Maps to: CC `services/tools/toolExecution.ts` `MessageUpdateLazy` as yielded
/// by concurrent tool execution before `runTools(...)` applies context
/// modifiers.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageUpdateLazy {
    pub message: Option<RenderableMessage>,
    pub tool_result: Option<UserMessage>,
    pub permission_request: Option<PermissionRequest>,
    pub blocked_on_permission: bool,
    pub forced_choice: Option<PermissionPromptChoice>,
    pub context_modifier: Option<ToolContextModifier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolExecutionUpdate {
    pub blocked_on_permission: bool,
    pub request: Option<PermissionRequest>,
    /// Immediate user-facing message yielded by `runToolUse(...)` before any
    /// permission dialog (for example official unknown-tool errors).
    pub message: Option<RenderableMessage>,
    /// Typed model `tool_result` paired with `message`, owned by tool execution
    /// so `query.rs` only forwards it into continuation history.
    pub tool_result: Option<UserMessage>,
    /// Maps to CC `PermissionDecision` returned without opening the interactive
    /// permission dialog (for example a configured deny rule).
    pub forced_choice: Option<PermissionPromptChoice>,
}

#[derive(Clone, Debug)]
pub struct PreToolUsePrepareResult {
    pub request: PermissionRequest,
    pub hook_messages: Vec<RenderableMessage>,
    pub hook_model_messages: Vec<Message>,
    pub forced_choice: Option<PermissionPromptChoice>,
    pub force_ask: bool,
    /// Maps to the independent `shouldPreventContinuation` and `stopReason`
    /// variables in CC `checkPermissionsAndCallTool(...)`.
    pub prevent_continuation: bool,
    pub stop_reason: Option<String>,
    /// Maps to CC `PermissionContext.handleHookAllow(...)` applying
    /// `updatedPermissions` to the in-session ToolPermissionContext.
    pub permission_updates: Vec<PermissionUpdate>,
    /// Maps to CC `hookPermissionResult.updatedInput !== undefined` on a hook
    /// ALLOW decision (`toolHooks.ts:520-527` attaches `updatedInput` to the
    /// allow result; `toolHooks.ts:353-354` reads it as
    /// `interactionSatisfied`). False for deny/ask/no-decision and for the
    /// passthrough `hookUpdatedInput` case (no permission decision).
    pub hook_supplied_updated_input: bool,
}

#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub message: Option<RenderableMessage>,
    /// Backwards-compatible aggregate of pre- and post-tool hook messages.
    /// Query/orchestration use the split fields below to preserve official
    /// yield ordering; tests can still assert hook execution via this field.
    pub hook_messages: Vec<RenderableMessage>,
    /// Messages yielded before the tool_result — the slice of CC's single
    /// `resultingMessages` array pushed ahead of the primary result: the
    /// `runPreToolUseHooks(...)` output before `tool.call(...)`, plus the
    /// `hook_permission_decision` row CC pushes at `toolExecution.ts:985`.
    pub pre_tool_messages: Vec<RenderableMessage>,
    /// The model/history half of the same slice: PreToolUse additional-context
    /// attachments and the `hook_permission_decision` attachment.
    pub pre_tool_model_messages: Vec<Message>,
    /// Messages yielded after the tool_result.
    /// Maps to CC `runPostToolUseHooks(...)` and trailing hook results.
    pub post_tool_messages: Vec<RenderableMessage>,
    /// Model-visible PostToolUse additional-context attachments.
    pub post_tool_model_messages: Vec<Message>,
    /// Maps to CC `ToolResult.newMessages`, yielded after the primary
    /// tool_result and successful PostToolUse hook results.
    pub new_messages: Vec<Message>,
    /// PreToolUse `continue: false` attachment, emitted only after an allowed
    /// tool has executed and after `ToolResult.newMessages`.
    pub continuation_messages: Vec<Message>,
    pub new_context: ToolUseContext,
    /// The APPLIED dialog choice (`apply_prompt_response` /
    /// `decision_for_choice`), not CC's `PermissionDecision` — that union now
    /// travels the `CanUseToolCallback` pipe (#156). This transport keeps the
    /// choice + updates the queue owner applied; see
    /// `types/permissions.rs#PromptDecision` for why it survives.
    pub permission_decision: PromptDecision,
    pub tool_result: Option<UserMessage>,
    /// Maps to CC `toolExecution.ts:1465-1470`: `addToolResult` attaches
    /// `{toolUseID, modifyContext: toolContextModifier}` to the pushed
    /// `MessageUpdateLazy` so orchestration can apply it to the context
    /// threaded into every subsequent tool use.
    pub context_modifier: Option<ToolContextModifier>,
}

/// Maps to: CC `services/tools/toolExecution.ts` `runToolUse(...)`.
///
/// The boundary is typed like the official path: it accepts a model
/// `ToolUseBlock`, receives the containing `AssistantMessage`, evaluates
/// `canUseTool`, and returns a permission/update object owned by tool execution
/// rather than `query.rs`.
pub fn run_tool_use(
    tool_use: &ToolUseBlock,
    assistant_message: &AssistantMessage,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> ToolExecutionUpdate {
    let tool_use = ToolUsePermissionRequest::from_tool_use_block(tool_use);
    run_tool_use_permission_gate_for_request_with_assistant(
        &tool_use,
        Some(assistant_message),
        context,
        permission_queue,
        false,
    )
}

pub fn run_tool_use_permission_gate(
    message: &RenderableMessage,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> ToolExecutionUpdate {
    let skip = ToolExecutionUpdate {
        blocked_on_permission: false,
        request: None,
        message: None,
        tool_result: None,
        forced_choice: None,
    };
    // Cometix-only entry: CC runs the permission gate once inside `runToolUse`,
    // before the tool starts, and never re-gates a transcript row. This entry
    // takes a `RenderableMessage`, so it can be handed a tool use that already
    // ran — re-prompting for it would be wrong.
    //
    // The guard belongs here, at the seam that has no CC counterpart. It used
    // to live inside `has_permissions_to_use_tool_inner` as an early `Allow`,
    // where CC's `permissions.ts:1158-1166` has no such branch: any caller
    // reporting a non-queued tool use skipped deny rules, ask rules, and the
    // classifier outright.
    let Some(tool_use) = ToolUsePermissionRequest::from_renderable_message(message) else {
        return skip;
    };
    // The row no longer carries a status to read, which is the point: liveness
    // is set membership (CC `Tool.ts:227` / `REPL.tsx:1897`). A tool already in
    // the live set has been gated and started, so re-gating it would prompt
    // twice for one execution.
    if context
        .in_progress_tool_use_ids
        .contains(&tool_use.tool_use_id)
    {
        return skip;
    }
    run_tool_use_permission_gate_for_request(&tool_use, context, permission_queue)
}

fn run_tool_use_permission_gate_for_request(
    tool_use: &ToolUsePermissionRequest,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
) -> ToolExecutionUpdate {
    run_tool_use_permission_gate_for_request_with_assistant(
        tool_use,
        None,
        context,
        permission_queue,
        true,
    )
}

pub(crate) fn merge_forced_permission_choices(
    first: Option<PermissionPromptChoice>,
    second: Option<PermissionPromptChoice>,
) -> Option<PermissionPromptChoice> {
    if first == Some(PermissionPromptChoice::Deny) || second == Some(PermissionPromptChoice::Deny) {
        Some(PermissionPromptChoice::Deny)
    } else {
        first.or(second)
    }
}

/// The port's carrier for CC `resolveHookPermissionDecision`'s
/// `{ decision, input }` (`services/tools/toolHooks.ts:340-343`), reshaped for
/// the query actor: the decision's input rides `request`, its behavior rides
/// `forced_choice` / `force_ask`.
///
/// Deliberately NOT as wide as `PermissionAllowDecision`
/// (`types/permissions.ts:174-184`) on the allow side. CC's decision object
/// does reach `toolExecution.ts` with every allow field — but the allow that
/// crosses the `canUseTool` boundary is REBUILT, not forwarded
/// (`hooks/useCanUseTool.tsx:113-134`):
///
/// ```text
/// if (result.behavior === 'allow') {
///   …
///   resolve(
///     ctx.buildAllow(result.updatedInput ?? input, {
///       decisionReason: result.decisionReason,
///     }),
///   )
/// ```
///
/// and `buildAllow` (`hooks/toolPermission/PermissionContext.ts:264-284`) emits
/// exactly `{ behavior:'allow', updatedInput, userModified:false,
/// decisionReason? }`. A gate/rule/mode/force allow therefore loses `toolUseID`,
/// `acceptFeedback` and `contentBlocks` inside CC itself, at precisely the layer
/// the three `CanUseToolResult::Allow { updated_input, .. }` consumers below sit
/// at.
///
/// **#185 ownership ruling — the strip is applied one layer UP.**
/// `hooks/use_can_use_tool.rs#forced_decision_to_can_use_tool_result` now
/// performs CC's `buildAllow` rebuild at its own boundary: it keeps
/// `updated_input` + `decision_reason`, pins `user_modified: Some(false)`, and
/// zeroes `tool_use_id` / `accept_feedback` / `content_blocks`. Before that
/// ruling the projection forwarded all six (#170) and these consumers' `..` was
/// LOAD-BEARING — correct behavior depended on each consumer remembering to
/// discard. It is now merely defensive: nothing arrives in those three fields
/// to discard, and the `..` only absorbs `user_modified`, which no consumer
/// reads. Read the two comments together; do not "fix" one without the other.
/// (issue-4 already misread this `..` once as a lossy flatten.)
///
/// Field by field:
///
/// - `updatedInput` → folded onto `request` (CC `getUpdatedInputOrFallback`,
///   `utils/permissions/permissions.ts:1477`), read back at
///   `toolExecution.ts:1130-1132`.
/// - `userModified` (`toolExecution.ts:1212`) → recomputed from the permission
///   RESPONSE by `permission_response_user_modified`, mirroring CC's own
///   `tool.inputsEquivalent` comparison inside `handleUserAllow`
///   (`PermissionContext.ts:308-309`). `buildAllow` pins `false` for every
///   non-dialog allow, which is what a `PermissionPromptResponse` carrying no
///   `updated_input` yields here.
/// - `decisionReason` → its only allow-side CC readers are
///   `decisionReasonToOTelSource` (`toolExecution.ts:958-961`, analytics) and
///   the `hook_permission_decision` attachment (`:980-993`). That attachment
///   now HAS a producer ([`hook_permission_decision_attachment`]), and it reads
///   the reason off `PermissionRequest.decision_reason` /
///   `PermissionPromptResponse.decision_reason` — the two carriers CC's single
///   decision object is split across here — not off this struct. The allow
///   legs of [`apply_required_can_use_tool_after_hooks`] install the arriving
///   allow's reason onto the request for exactly that reason, mirroring
///   `buildAllow`'s `...(opts?.decisionReason && { decisionReason })`
///   (`PermissionContext.ts:277`).
/// - `toolUseID` → no reader anywhere in the flow. ast-grep
///   `permissionDecision.$FIELD` over `services/tools/toolExecution.ts` yields
///   behavior / decisionReason / message / updatedInput / userModified /
///   acceptFeedback / contentBlocks and nothing else; the id the flow uses is
///   the `toolUseID` parameter, and `request.tool_use_id` already carries it.
///   #170 kept it because the SDK permission-prompt normalizer really does
///   produce one — but that producer terminates in
///   `PermissionPromptResponse::with_tool_use_id` (`cli/structured_io.rs:782-793`),
///   never in a `force_decision`, so it cannot reach this layer either way.
///   Census in `hooks/use_can_use_tool.rs`'s allow arm (#185).
/// - `acceptFeedback` (`toolExecution.ts:1421-1429`) and the allow-side
///   `contentBlocks` (`:1431-1438`) → only ever set by a dialog/leader
///   RESOLUTION: `PermissionContext.ts:291-317#handleUserAllow` (callers
///   `interactiveHandler.ts:173-180`, `swarmWorkerHandler.ts:100-106`) and the
///   in-process teammate answer (`inProcessRunner.ts:283-290`). In this port
///   all three resolve a `PermissionPromptResponse` instead, whose `feedback`
///   and `content_blocks` feed the same append chain
///   (`append_accept_feedback_to_tool_result` /
///   `append_permission_content_blocks`). Widening this struct for them would
///   duplicate that carrier, not repair a drop.
pub struct RequiredCanUseToolResult {
    pub request: PermissionRequest,
    pub forced_choice: Option<PermissionPromptChoice>,
    pub force_ask: bool,
    pub permission_updates: Vec<crate::types::permissions::PermissionUpdate>,
}

/// Maps to CC `resolveHookPermissionDecision(...)`: PreToolUse hooks run first,
/// then the final processed input is passed through `canUseTool`. Nested/forked
/// callers use their required callback; normal callers re-run the shared
/// settings/classifier gate so a hook rewrite cannot inherit an earlier grant.
///
/// `hook_supplied_updated_input` is CC's
/// `hookPermissionResult.updatedInput !== undefined` on the hook ALLOW decision
/// (`toolHooks.ts:353-354`): a hook allow that carries `updatedInput` for an
/// interactive tool IS the user interaction (`interactionSatisfied`), so the
/// duplicate `canUseTool` re-prompt is skipped and only deny/ask rules re-check.
pub async fn apply_required_can_use_tool_after_hooks(
    mut request: PermissionRequest,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
    hook_choice: Option<PermissionPromptChoice>,
    hook_supplied_updated_input: bool,
) -> Result<RequiredCanUseToolResult, crate::utils::errors::AbortError> {
    let Some(tool) = crate::types::tools::find_tool_by_name(&context.tools, &request.tool_name)
    else {
        return Ok(RequiredCanUseToolResult {
            request,
            forced_choice: Some(PermissionPromptChoice::Deny),
            force_ask: false,
            permission_updates: Vec::new(),
        });
    };
    if hook_choice == Some(PermissionPromptChoice::Deny) {
        return Ok(RequiredCanUseToolResult {
            request,
            forced_choice: hook_choice,
            force_ask: false,
            permission_updates: Vec::new(),
        });
    }
    let hook_allowed = matches!(
        hook_choice,
        Some(PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow)
    );
    let requires_interaction = find_tool_call(&request.tool_name)
        .is_some_and(crate::tool::ToolCall::requires_user_interaction);
    let interaction_satisfied = requires_interaction && hook_supplied_updated_input;
    let required_callback = context.require_can_use_tool
        || (hook_allowed && requires_interaction && !interaction_satisfied);
    if hook_allowed && !required_callback {
        // CC toolHooks.ts:372-405: null/deny finish here; an ask re-enters
        // canUseTool without forceDecision, after the read-only rule check.
        let decision = crate::utils::permissions::permissions::check_rule_based_permissions(
            tool,
            &request.input,
            context,
        )
        .await;
        match decision {
            None => {
                pin_user_approved_edit_destination(&mut request, context);
                return Ok(RequiredCanUseToolResult {
                    request,
                    forced_choice: hook_choice,
                    force_ask: false,
                    permission_updates: Vec::new(),
                });
            }
            Some(decision @ PermissionDecision::Deny { .. }) => {
                if let PermissionDecision::Deny {
                    message,
                    decision_reason,
                    ..
                } = &decision
                {
                    request.message = message.clone();
                    request.decision_reason = Some(decision_reason.clone());
                }
                request.permission_result = Some(decision);
                return Ok(RequiredCanUseToolResult {
                    request,
                    forced_choice: Some(PermissionPromptChoice::Deny),
                    force_ask: false,
                    permission_updates: Vec::new(),
                });
            }
            Some(PermissionDecision::Ask { .. }) => {}
            Some(PermissionDecision::Allow { .. }) => unreachable!("rule-only check cannot allow"),
        }
    }
    // CC toolHooks.ts:419-432: only a hook's own ask is a forceDecision;
    // the rule-only ask above rechecks without one.
    let force_decision = if !hook_allowed {
        request.permission_result.clone().filter(|decision| matches!(decision,
            PermissionDecision::Ask { decision_reason: Some(crate::types::permissions::PermissionDecisionReason::Hook { hook_name, .. }), .. }
                if hook_name == "PreToolUse" || hook_name.starts_with("PreToolUse:")))
    } else {
        None
    };
    // CC toolHooks.ts:356-365,396-403: a rejected Promise stays an error up to runToolUse.
    let callback_decision = if let Some(assistant) = assistant_message {
        context
            .can_use_tool
            .decide_async(
                tool,
                &request.input,
                context,
                assistant,
                &request.tool_use_id,
                force_decision.clone(),
            )
            .await?
    } else {
        None
    };
    if required_callback && callback_decision.is_none() {
        return Ok(RequiredCanUseToolResult {
            request,
            forced_choice: Some(PermissionPromptChoice::Deny),
            force_ask: false,
            permission_updates: Vec::new(),
        });
    }
    if let Some(decision) = callback_decision.as_ref() {
        let updated_input = match decision {
            PermissionDecision::Allow { updated_input, .. }
            | PermissionDecision::Ask { updated_input, .. } => updated_input.clone(),
            PermissionDecision::Deny { .. } => None,
        };
        if let Some(input) = updated_input {
            request = permission_request_with_updated_input(request, input);
        }
    }
    if required_callback {
        let decision = callback_decision.expect("required callback presence checked");
        match &decision {
            PermissionDecision::Deny {
                message,
                decision_reason,
                ..
            } => {
                request.message = message.clone();
                request.decision_reason = Some(decision_reason.clone());
            }
            PermissionDecision::Ask {
                message,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                ..
            } => {
                request.message = message.clone();
                request.decision_reason = decision_reason.clone();
                request.suggestions = suggestions.clone();
                request.blocked_path = blocked_path.clone();
                request.metadata = metadata.clone();
                request.is_compound_command = matches!(
                    decision_reason,
                    Some(
                        crate::types::permissions::PermissionDecisionReason::SubcommandResults { .. }
                    )
                );
            }
            PermissionDecision::Allow {
                decision_reason, ..
            } => request.decision_reason = decision_reason.clone(),
        }
        let (forced_choice, force_ask) = match &decision {
            PermissionDecision::Deny { .. } => (Some(PermissionPromptChoice::Deny), false),
            PermissionDecision::Ask { .. } if context.abort_controller.is_aborted() => {
                (Some(PermissionPromptChoice::Deny), false)
            }
            PermissionDecision::Ask { .. } => (None, true),
            PermissionDecision::Allow { .. } => (
                merge_forced_permission_choices(
                    hook_choice,
                    Some(PermissionPromptChoice::AllowOnce),
                ),
                false,
            ),
        };
        request.permission_result = Some(decision);
        pin_user_approved_edit_destination(&mut request, context);
        return Ok(RequiredCanUseToolResult {
            request,
            forced_choice,
            force_ask,
            permission_updates: Vec::new(),
        });
    }
    let tool_use = ToolUsePermissionRequest {
        tool_use_id: request.tool_use_id.clone(),
        tool_name: request.tool_name.clone(),
        input_summary: permission_input_summary(&request.tool_name, &request.input),
        input: request.input.clone(),
    };
    let mut ignored_queue = Vec::new();
    let result = can_use_tool_or_queue_permission_for_params_with_store(
        CanUseToolParams::from_tool_use_context(
            context,
            Some(tool),
            &tool_use,
            &request.tool_use_id,
            assistant_message,
            callback_decision.or(force_decision),
        ),
        &mut ignored_queue,
        context.app_store.store.as_ref(),
    );
    let (mut request, forced_choice, force_ask) = match result {
        CanUseToolResult::Aborted(error) => return Err(error),
        CanUseToolResult::Deny(mut result) => {
            result.call_input = request.call_input;
            (result, Some(PermissionPromptChoice::Deny), false)
        }
        CanUseToolResult::Ask(mut result) => {
            result.call_input = request.call_input;
            // CC useCanUseTool may resolve Ask after cancelAndAbort. That is a
            // completed refusal, not a still-pending interactive prompt.
            if context.abort_controller.is_aborted() {
                (result, Some(PermissionPromptChoice::Deny), false)
            } else {
                (result, None, true)
            }
        }
        CanUseToolResult::Allow {
            updated_input,
            decision_reason,
            ..
        } => {
            if let Some(input) = updated_input {
                request = permission_request_with_updated_input(request, input);
            }
            request.decision_reason = decision_reason;
            (request, Some(PermissionPromptChoice::AllowOnce), false)
        }
    };
    pin_user_approved_edit_destination(&mut request, context);
    Ok(RequiredCanUseToolResult {
        request,
        forced_choice,
        force_ask,
        permission_updates: Vec::new(),
    })
}

fn run_tool_use_permission_gate_for_request_with_assistant(
    tool_use: &ToolUsePermissionRequest,
    assistant_message: Option<&AssistantMessage>,
    context: &ToolUseContext,
    permission_queue: &mut Vec<ToolUseConfirm>,
    evaluate_permission_before_hooks: bool,
) -> ToolExecutionUpdate {
    // Maps to: CC `runToolUse` abort gate at toolExecution.ts:415 — refuse to
    // start permission/hooks/tool work when the turn is already cancelled.
    if context.abort_controller.is_aborted() {
        // The cancelled row records the bare sentinel as its raw
        // `toolUseResult` for every tool (CC `toolExecution.ts:448`).
        let message = RenderableMessage::user_tool_result(
            uuid::Uuid::new_v4().to_string(),
            tool_use.tool_use_id.clone(),
            crate::utils::messages::CANCEL_MESSAGE,
            true,
        )
        .with_tool_use_result(Some(serde_json::Value::String(
            crate::utils::messages::CANCEL_MESSAGE.to_string(),
        )));
        let mut tool_result =
            transcript_tool_result_to_model_message(&message, &tool_use.tool_name);
        // CC `toolExecution.ts:449`: the cancelled result records its
        // assistant source.
        stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
        return ToolExecutionUpdate {
            blocked_on_permission: false,
            request: None,
            message: Some(message),
            tool_result,
            forced_choice: None,
        };
    }
    let active_tool_definition =
        crate::types::tools::find_tool_by_name(&context.tools, &tool_use.tool_name);
    let base_tools = crate::tools::get_all_base_tools();
    let fallback_alias_definition =
        crate::types::tools::find_tool_by_name(&base_tools, &tool_use.tool_name).filter(|tool| {
            tool.aliases
                .iter()
                .any(|alias| alias == &tool_use.tool_name)
        });
    let tool_definition = active_tool_definition.or(fallback_alias_definition);
    if tool_definition.is_none() {
        let message = unknown_tool_result(tool_use);
        let mut tool_result =
            transcript_tool_result_to_model_message(&message, &tool_use.tool_name);
        // CC `toolExecution.ts:486` (unknown-tool error result).
        stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
        return ToolExecutionUpdate {
            blocked_on_permission: false,
            request: None,
            message: Some(message),
            tool_result,
            forced_choice: None,
        };
    }
    if tool_definition
        .is_some_and(|tool| tool.name == crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME)
        && !crate::utils::env_utils::is_cometix_write_enabled()
    {
        let content = crate::tools::shared::write_gate::FILE_EDIT_DISABLED_ERROR.to_string();
        // CC's internal `toolUseResult: "Error: ..."` string rides the
        // row itself; the row carries no display shape.
        let message = RenderableMessage::user_tool_result(
            uuid::Uuid::new_v4().to_string(),
            tool_use.tool_use_id.clone(),
            content.clone(),
            true,
        )
        .with_tool_use_result(Some(serde_json::Value::String(format!("Error: {content}"))));
        let tool_result = transcript_tool_result_to_model_message(
            &message,
            crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME,
        );
        return ToolExecutionUpdate {
            blocked_on_permission: false,
            request: None,
            message: Some(message),
            tool_result,
            forced_choice: None,
        };
    }
    let mut parsed_tool_use = tool_use.clone();
    let mut call_input = None;
    if let Some(tool) = tool_definition {
        if !is_legacy_summary_only_tool_input(tool_use) {
            // Maps to: CC `services/tools/toolExecution.ts:615-676`.
            // First parse the schema-owned input and preserve it as
            // `callInput`. Read validates that initial value before backfill;
            // observers receive a separate absolute-path clone afterward.
            let mut parsed_input = find_tool_call(&tool.name)
                .map(|tool_call| tool_call.normalize_input_with_context(&tool_use.input, context))
                .unwrap_or_else(|| tool_use.input.clone());
            match tool.input_zod_schema.as_ref().map_or_else(
                || validate_tool_input_for_execution(&tool.name, &parsed_input, &tool.input_schema),
                |schema| carrier_parsed_input(&tool.name, schema.0, &parsed_input).map(Some),
            ) {
                Err(error) => {
                    let formatted =
                        match build_schema_not_sent_hint(tool, &context.messages, &context.tools) {
                            Some(hint) => format!("{}{hint}", error.formatted),
                            None => error.formatted,
                        };
                    let message = input_validation_error_result(tool_use, &formatted, &error.raw);
                    let mut tool_result =
                        transcript_tool_result_to_model_message(&message, &tool.name);
                    // CC `toolExecution.ts:676` (input validation error result).
                    stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
                    return ToolExecutionUpdate {
                        blocked_on_permission: false,
                        request: None,
                        message: Some(message),
                        tool_result,
                        forced_choice: None,
                    };
                }
                // CC `toolExecution.ts:761` `processedInput = parsedInput.data`
                // — the parse result (unknown fields stripped) replaces the
                // input for permission, validation, and execution.
                Ok(Some(data)) => parsed_input = data,
                Ok(None) => {}
            }
            call_input = Some(parsed_input.clone());
            if let Some(tool_call) = find_tool_call(&tool.name) {
                // CC validates the strict initially parsed Read input before
                // creating the observable absolute-path clone. Other tools
                // retain their existing slice-owned observable validation.
                let validation_input = if tool_call.name() == "Read" {
                    parsed_input.clone()
                } else {
                    tool_call.backfill_observable_input(&parsed_input, context)
                };
                let validation_message = match tool_call.validate_input(&validation_input, context)
                {
                    crate::tool::ValidationResult::Ok => None,
                    crate::tool::ValidationResult::Error {
                        message: validation_message,
                        error_code,
                    } => Some(semantic_input_validation_error_result(
                        tool_use,
                        &validation_message,
                        error_code,
                    )),
                    crate::tool::ValidationResult::Fatal {
                        message: validation_message,
                    } => Some(fatal_input_validation_error_result(
                        tool_use,
                        &tool.name,
                        &validation_message,
                    )),
                };
                if let Some(message) = validation_message {
                    let mut tool_result =
                        transcript_tool_result_to_model_message(&message, &tool.name);
                    // CC `toolExecution.ts:729` (semantic validation error).
                    stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
                    return ToolExecutionUpdate {
                        blocked_on_permission: false,
                        request: None,
                        message: Some(message),
                        tool_result,
                        forced_choice: None,
                    };
                }
                parsed_tool_use.input = if tool_call.name() == "Read" {
                    tool_call.backfill_observable_input(&parsed_input, context)
                } else {
                    validation_input
                };
            } else {
                parsed_tool_use.input = parsed_input;
            }
            parsed_tool_use.input_summary =
                permission_input_summary(&parsed_tool_use.tool_name, &parsed_tool_use.input);
        }
    }
    // Official `runToolUse` runs PreToolUse before canUseTool. Typed
    // production orchestration therefore returns the parsed request here and
    // lets `prepare_permission_request_before_prompt` +
    // `apply_required_can_use_tool_after_hooks` evaluate the final hook input.
    // The renderable-message compatibility gate below may still evaluate
    // immediately because it has no async hook continuation owner.
    if !evaluate_permission_before_hooks {
        let effective_tool_use = parsed_tool_use;
        let mut call_input = call_input;
        let checked_file_destination = match effective_tool_use.tool_name.as_str() {
            crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME => effective_tool_use
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .map(std::path::Path::new)
                .map(|path| {
                    (
                        crate::tools::file_write_tool::CHECKED_WRITE_DESTINATION_KEY,
                        crate::tools::file_write_tool::resolved_write_destination(path),
                    )
                }),
            crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME => effective_tool_use
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .map(std::path::Path::new)
                .map(|path| {
                    (
                        crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY,
                        crate::tools::file_edit_tool::resolved_edit_destination(path),
                    )
                }),
            crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME => {
                effective_tool_use
                    .input
                    .get("notebook_path")
                    .and_then(serde_json::Value::as_str)
                    .map(|path| {
                        let expanded =
                            crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                                .unwrap_or_else(|_| std::path::PathBuf::from(path));
                        (
                            crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY,
                            crate::tools::notebook_edit_tool::resolved_notebook_destination(
                                &expanded,
                            ),
                        )
                    })
            }
            _ => None,
        }
        .map(|(key, path)| (key, path.display().to_string()));
        if let (Some((key, destination)), Some(call_input)) =
            (checked_file_destination, call_input.as_mut())
        {
            if let Some(object) = call_input.as_object_mut() {
                object.insert(key.to_string(), serde_json::Value::String(destination));
                if effective_tool_use.tool_name == crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME
                {
                    if let Some(path) = effective_tool_use
                        .input
                        .get("file_path")
                        .and_then(serde_json::Value::as_str)
                    {
                        object.insert(
                            crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY
                                .to_string(),
                            serde_json::Value::String(path.to_string()),
                        );
                    }
                } else if effective_tool_use.tool_name
                    == crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME
                {
                    if let Some(path) = effective_tool_use
                        .input
                        .get("notebook_path")
                        .and_then(serde_json::Value::as_str)
                    {
                        let expanded =
                            crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                                .unwrap_or_else(|_| std::path::PathBuf::from(path));
                        object.insert(
                            crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_APPROVED_PATH_KEY
                                .to_string(),
                            serde_json::Value::String(expanded.display().to_string()),
                        );
                    }
                }
            }
        }
        let mut request = mock_permission_request_with_input(
            format!("perm-{}", effective_tool_use.tool_use_id),
            effective_tool_use.tool_use_id,
            effective_tool_use.tool_name,
            effective_tool_use.input_summary,
            effective_tool_use.input,
            context.tool_permission_context.mode,
        );
        request.call_input = call_input;
        return ToolExecutionUpdate {
            // This means "await final post-hook permission resolution", not
            // necessarily "show an interactive prompt". The async owner may
            // immediately resolve Allow/Deny without surfacing UI.
            blocked_on_permission: true,
            request: Some(request),
            message: None,
            tool_result: None,
            forced_choice: None,
        };
    }

    // Compatibility permission gate. Classifier may `block_on` here —
    // R3b debt: prefer `can_use_tool_async` once orchestration is async.
    // Maps to: CC `await canUseTool(...)` / `await hasPermissionsToUseTool`.
    // Forked agents supply their own exact `CanUseToolFn`; it must run even
    // when normal settings would auto-approve a read-only or bypass tool.
    let callback_decision = (!context.require_can_use_tool)
        .then(|| tool_definition.zip(assistant_message))
        .flatten()
        .map(|(tool, assistant_message)| {
            context.can_use_tool.decide(
                tool,
                &parsed_tool_use.input,
                context,
                assistant_message,
                &parsed_tool_use.tool_use_id,
                None,
            )
        })
        .transpose();
    let callback_decision = match callback_decision {
        Ok(decision) => decision.flatten(),
        Err(error) => {
            return permission_check_error_result(
                &parsed_tool_use.tool_use_id,
                &parsed_tool_use.tool_name,
                assistant_message,
                &error,
            );
        }
    };
    // CC CanUseToolFn may rewrite input (speculation overlay paths). The
    // rewritten request must flow through hooks, permission UI, and tool.call.
    let mut effective_tool_use = parsed_tool_use;
    if let Some(updated_input) = callback_decision
        .as_ref()
        .and_then(|decision| match decision {
            crate::types::permissions::PermissionDecision::Allow { updated_input, .. }
            | crate::types::permissions::PermissionDecision::Ask { updated_input, .. } => {
                updated_input.clone()
            }
            crate::types::permissions::PermissionDecision::Deny { .. } => None,
        })
    {
        effective_tool_use.input = updated_input;
        effective_tool_use.input_summary =
            permission_input_summary(&effective_tool_use.tool_name, &effective_tool_use.input);
    }
    let checked_file_destination = match effective_tool_use.tool_name.as_str() {
        crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME => effective_tool_use
            .input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(std::path::Path::new)
            .map(|path| {
                (
                    crate::tools::file_write_tool::CHECKED_WRITE_DESTINATION_KEY,
                    crate::tools::file_write_tool::resolved_write_destination(path),
                )
            }),
        crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME => effective_tool_use
            .input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(std::path::Path::new)
            .map(|path| {
                (
                    crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY,
                    crate::tools::file_edit_tool::resolved_edit_destination(path),
                )
            }),
        crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME => effective_tool_use
            .input
            .get("notebook_path")
            .and_then(serde_json::Value::as_str)
            .map(|path| {
                let expanded =
                    crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                        .unwrap_or_else(|_| std::path::PathBuf::from(path));
                (
                    crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY,
                    crate::tools::notebook_edit_tool::resolved_notebook_destination(&expanded),
                )
            }),
        _ => None,
    }
    .map(|(key, path)| (key, path.display().to_string()));
    let fallback_request = mock_permission_request_with_input(
        format!("perm-{}", effective_tool_use.tool_use_id),
        effective_tool_use.tool_use_id.clone(),
        effective_tool_use.tool_name.clone(),
        effective_tool_use.input_summary.clone(),
        effective_tool_use.input.clone(),
        context.tool_permission_context.mode,
    );
    let result = can_use_tool_or_queue_permission_for_params_with_store(
        CanUseToolParams::from_tool_use_context(
            context,
            tool_definition,
            &effective_tool_use,
            &effective_tool_use.tool_use_id,
            assistant_message,
            callback_decision,
        ),
        permission_queue,
        context.app_store.store.as_ref(),
    );
    if let CanUseToolResult::Aborted(error) = &result {
        return permission_check_error_result(
            &effective_tool_use.tool_use_id,
            &effective_tool_use.tool_name,
            assistant_message,
            error,
        );
    }
    let cancelled_ask =
        matches!(result, CanUseToolResult::Ask(_)) && context.abort_controller.is_aborted();
    let blocked_on_permission = matches!(result, CanUseToolResult::Ask(_)) && !cancelled_ask;
    let forced_choice = match result {
        CanUseToolResult::Deny(_) => Some(PermissionPromptChoice::Deny),
        CanUseToolResult::Ask(_) if cancelled_ask => Some(PermissionPromptChoice::Deny),
        _ => None,
    };
    let mut request = match result {
        CanUseToolResult::Aborted(_) => {
            unreachable!("rejection handled before decision projection")
        }
        CanUseToolResult::Ask(request) | CanUseToolResult::Deny(request) => Some(request),
        // An allow's `updatedInput` is what executes (CC
        // `getUpdatedInputOrFallback`); fold it onto the fallback request.
        //
        // The remaining allow fields have no producer on THIS leg at all:
        // `evaluate_permission_before_hooks == true` happens only on the
        // `assistant_message: None` entry
        // (`run_tool_use_permission_gate_for_request`), so the
        // `callback_decision` computed above is always `None` and no forced
        // decision ever reaches `forced_decision_to_can_use_tool_result`. What
        // arrives is an engine allow, whose `tool_use_id` / `accept_feedback` /
        // `content_blocks` are empty by construction. CC strips those three off
        // an allow at this layer anyway (`useCanUseTool.tsx:129-133` →
        // `buildAllow`); see [`RequiredCanUseToolResult`].
        CanUseToolResult::Allow { updated_input, .. } => Some(match updated_input {
            Some(updated_input) => {
                permission_request_with_updated_input(fallback_request, updated_input)
            }
            None => fallback_request,
        }),
    };
    if let Some(request) = request.as_mut() {
        if let (Some((key, destination)), Some(call_input)) =
            (checked_file_destination, call_input.as_mut())
        {
            if let Some(object) = call_input.as_object_mut() {
                object.insert(key.to_string(), serde_json::Value::String(destination));
                if effective_tool_use.tool_name == crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME
                {
                    if let Some(path) = effective_tool_use
                        .input
                        .get("file_path")
                        .and_then(serde_json::Value::as_str)
                    {
                        object.insert(
                            crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY
                                .to_string(),
                            serde_json::Value::String(path.to_string()),
                        );
                    }
                } else if effective_tool_use.tool_name
                    == crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME
                {
                    if let Some(path) = effective_tool_use
                        .input
                        .get("notebook_path")
                        .and_then(serde_json::Value::as_str)
                    {
                        let expanded =
                            crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                                .unwrap_or_else(|_| std::path::PathBuf::from(path));
                        object.insert(
                            crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_APPROVED_PATH_KEY
                                .to_string(),
                            serde_json::Value::String(expanded.display().to_string()),
                        );
                    }
                }
            }
        }
        request.call_input = call_input;
    }
    ToolExecutionUpdate {
        blocked_on_permission,
        request,
        message: None,
        tool_result: None,
        forced_choice,
    }
}

/// Maps to CC `toolExecution.ts:395-406`: the unknown-tool row carries the
/// bare `Error: No such tool available: {name}` string as its `toolUseResult`.
fn unknown_tool_result(tool_use: &ToolUsePermissionRequest) -> RenderableMessage {
    let error = format!("Error: No such tool available: {}", tool_use.tool_name);
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        tool_use.tool_use_id.clone(),
        format!("<tool_use_error>{error}</tool_use_error>"),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(error)))
}

/// Maps to CC `toolExecution.ts:469-489`: permission Promise rejections are
/// caught by runToolUse before tool.call's own try/catch (which starts at :1206).
/// L1 projection into the actor's existing row/model-result transport.
pub(crate) fn permission_check_error_result(
    tool_use_id: &str,
    tool_name: &str,
    assistant_message: Option<&AssistantMessage>,
    error: &crate::utils::errors::AbortError,
) -> ToolExecutionUpdate {
    let detailed_error = format!("Error calling tool ({tool_name}): {error}");
    let message = RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        tool_use_id.to_string(),
        format!("<tool_use_error>{detailed_error}</tool_use_error>"),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(detailed_error)));
    let mut tool_result = transcript_tool_result_to_model_message(&message, tool_name);
    stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
    ToolExecutionUpdate {
        blocked_on_permission: false,
        request: None,
        message: Some(message),
        tool_result,
        forced_choice: None,
    }
}

/// Maps to CC `toolExecution.ts:668-676`: `toolUseResult` is the unformatted
/// zod message, while `content` carries the formatted one.
fn input_validation_error_result(
    tool_use: &ToolUsePermissionRequest,
    formatted_error: &str,
    raw_error: &str,
) -> RenderableMessage {
    let tool_use_result = format!("InputValidationError: {raw_error}");
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        tool_use.tool_use_id.clone(),
        format!("<tool_use_error>InputValidationError: {formatted_error}</tool_use_error>"),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(tool_use_result)))
}

/// Maps to CC `toolExecution.ts:474-486`: the detailed error is both the
/// wrapped `content` and the raw `toolUseResult`.
fn fatal_input_validation_error_result(
    tool_use: &ToolUsePermissionRequest,
    canonical_tool_name: &str,
    message: &str,
) -> RenderableMessage {
    let detailed_error = format!("Error calling tool ({canonical_tool_name}): {message}");
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        tool_use.tool_use_id.clone(),
        format!("<tool_use_error>{detailed_error}</tool_use_error>"),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(detailed_error)))
}

/// Maps to CC `toolExecution.ts:721-729`: a `validateInput` failure records
/// `Error: {message}` as the raw `toolUseResult`.
fn semantic_input_validation_error_result(
    tool_use: &ToolUsePermissionRequest,
    message: &str,
    _error_code: i32,
) -> RenderableMessage {
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        tool_use.tool_use_id.clone(),
        format!("<tool_use_error>{message}</tool_use_error>"),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(format!("Error: {message}"))))
}

/// Maps to CC `toolExecution.ts` `buildSchemaNotSentHint(...)`.
fn build_schema_not_sent_hint(
    tool: &crate::types::tools::Tool,
    messages: &[Message],
    tools: &[crate::types::tools::Tool],
) -> Option<String> {
    if !crate::tools::tool_search_tool::prompt::is_tool_search_enabled_optimistic() {
        return None;
    }
    if !crate::tools::tool_search_tool::prompt::is_tool_search_tool_available(tools) {
        return None;
    }
    if !crate::tools::tool_search_tool::prompt::is_deferred_tool(tool) {
        return None;
    }
    let discovered = crate::utils::tool_search::extract_discovered_tool_names(messages);
    if discovered.contains(&tool.name) {
        return None;
    }
    Some(format!(
        "\n\nThis tool's schema was not sent to the API — it was not in the discovered-tool set derived from message history. Without the schema in your prompt, typed parameters (arrays, numbers, booleans) get emitted as strings and the client-side parser rejects them. Load the tool first: call {} with query \"select:{}\", then retry this call.",
        crate::tools::tool_search_tool::prompt::TOOL_SEARCH_TOOL_NAME,
        tool.name,
    ))
}

fn permission_request_with_updated_input(
    mut request: PermissionRequest,
    input: serde_json::Value,
) -> PermissionRequest {
    request.input = input;
    request.input_summary = permission_input_summary(&request.tool_name, &request.input);
    request.rule = PermissionRuleValue::new(
        request.tool_name.clone(),
        Some(request.input_summary.clone()),
    );
    request
}

fn permission_input_summary(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" | "PowerShell" => input
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        "Read" => input
            .get("file_path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        "Write" | "FileWrite" | "Edit" | "FileEdit" | "NotebookEdit" => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .or_else(|| input.get("notebook_path"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => serde_json::to_string(input).unwrap_or_default(),
    }
}

/// Maps to the zod `tool.inputSchema.safeParse(input)` gate at the start of
/// CC `services/tools/toolExecution.ts#checkPermissionsAndCallTool`. Rust tool
/// metadata already stores API JSON schema; this validates the subset used by
/// Cometix built-ins before permission prompts or tool execution.
fn is_legacy_summary_only_tool_input(tool_use: &ToolUsePermissionRequest) -> bool {
    tool_use.input.as_object().is_some_and(|object| {
        object.len() == 1
            && object.contains_key("summary")
            && object.get("summary").and_then(|value| value.as_str())
                == Some(tool_use.input_summary.as_str())
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolInputSchemaValidationError {
    formatted: String,
    raw: String,
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Exact Zod-v4 issue projection for Glob's strict two-field schema.
/// Exact Zod-v4 issue projection for CC `GrepTool.inputSchema` after its
/// semantic number/boolean preprocessors have run.
fn tool_input_schema_error_from_issues(
    tool_name: &str,
    issues: Vec<serde_json::Value>,
    missing: Vec<String>,
    unexpected: Vec<String>,
    mismatches: Vec<(String, &'static str, &'static str)>,
) -> ToolInputSchemaValidationError {
    let mut parts = missing
        .into_iter()
        .map(|parameter| format!("The required parameter `{parameter}` is missing"))
        .collect::<Vec<_>>();
    parts.extend(
        unexpected
            .into_iter()
            .map(|parameter| format!("An unexpected parameter `{parameter}` was provided")),
    );
    parts.extend(mismatches.into_iter().map(|(parameter, expected, received)| {
        format!(
            "The parameter `{parameter}` type is expected as `{expected}` but provided as `{received}`"
        )
    }));
    let raw = serde_json::to_string_pretty(&issues).unwrap_or_default();
    let formatted = if parts.is_empty() {
        raw.clone()
    } else {
        format!(
            "{tool_name} failed due to the following {}:\n{}",
            if parts.len() > 1 { "issues" } else { "issue" },
            parts.join("\n")
        )
    };
    ToolInputSchemaValidationError { formatted, raw }
}

/// Run a tool's carrier schema and shape the failure the way CC's gate does:
/// `formatZodValidationError` for the model-facing copy, the issues dump for
/// `toolUseResult` (`toolExecution.ts:617,672`). `Ok` is CC's
/// `parsedInput.data` — strip/preprocess/default applied.
fn carrier_parsed_input(
    tool_name: &str,
    schema: &crate::utils::zod::Schema,
    input: &serde_json::Value,
) -> Result<serde_json::Value, ToolInputSchemaValidationError> {
    match crate::utils::zod::safe_parse(schema, input) {
        Ok(data) => Ok(data),
        Err(error) => Err(ToolInputSchemaValidationError {
            formatted: crate::utils::tool_errors::format_zod_validation_error(tool_name, &error),
            raw: error.message(),
        }),
    }
}

/// Maps to: CC `toolExecution.ts:615` `tool.inputSchema.safeParse(input)`.
///
/// `Ok(Some(data))` is `parsedInput.data` for a carrier tool whose downstream
/// input MUST be the parse result (CC `:761` `processedInput = parsedInput.data`)
/// — Agent's plain `z.object` ACCEPTS and STRIPS unknown fields, which the
/// JSON-Schema projection (`additionalProperties: false` on both object kinds,
/// zod/v4 oracle) cannot express: validating against it rejects what CC strips.
/// `Ok(None)` means the caller keeps its existing input: the Read/Glob/Grep
/// carriers validate here but their data flow was aligned per-tool (Read's
/// path-preservation contract at the call site), and the JSON-Schema fallback
/// has no parse result at all.
fn validate_tool_input_for_execution(
    tool_name: &str,
    input: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ToolInputSchemaValidationError> {
    if tool_name == crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME {
        return carrier_parsed_input("Read", crate::tools::file_read_tool::input_schema(), input)
            .map(|_| None);
    }
    if tool_name == crate::tools::glob_tool::prompt::GLOB_TOOL_NAME {
        // Carrier path (PORTING.md `Zod v4 runtime carrier`, clause 1): the
        // tool's one schema drives validation, and the model-facing copy comes
        // from the ported `formatZodValidationError` rather than a hand-written
        // per-tool formatter.
        return carrier_parsed_input("Glob", crate::tools::glob_tool::input_schema(), input)
            .map(|_| None);
    }
    if tool_name == crate::tools::grep_tool::prompt::GREP_TOOL_NAME {
        return carrier_parsed_input("Grep", crate::tools::grep_tool::input_schema(), input)
            .map(|_| None);
    }
    if tool_name == crate::tools::agent_tool::constants::AGENT_TOOL_NAME {
        return carrier_parsed_input("Agent", crate::tools::agent_tool::input_schema(), input)
            .map(Some);
    }
    validate_tool_input_against_schema(tool_name, input, schema)
        .map(|()| None)
        .map_err(|error| ToolInputSchemaValidationError {
            formatted: error.clone(),
            raw: error,
        })
}

pub(crate) fn validate_tool_input_against_schema(
    tool_name: &str,
    input: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    // Maps to CC `createSyntheticOutputTool`: Ajv compiles the caller schema
    // and validates every StructuredOutput invocation. The shared lightweight
    // validator below remains for static built-in schemas so their established
    // field-specific error copy does not change.
    if tool_name == crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME {
        let validator = jsonschema::draft7::new(schema)
            .map_err(|error| format!("invalid StructuredOutput schema: {error}"))?;
        // Maps to CC `SyntheticOutputTool.ts:145-151`: each error is
        // `${instancePath || 'root'}: ${message}`, joined with `, `.
        let errors = validator
            .iter_errors(input)
            .map(|error| {
                let instance_path = error.instance_path().to_string();
                let instance_path = if instance_path.is_empty() {
                    "root".to_string()
                } else {
                    instance_path
                };
                format!("{instance_path}: {error}")
            })
            .collect::<Vec<_>>();
        if errors.is_empty() {
            return Ok(());
        }
        return Err(format!(
            "Output does not match required schema: {}",
            errors.join(", ")
        ));
    }

    if schema.get("type").and_then(|value| value.as_str()) == Some("object") && !input.is_object() {
        return Err(format!("{tool_name} input must be an object"));
    }

    let Some(input_object) = input.as_object() else {
        return Ok(());
    };
    let properties = schema.get("properties").and_then(|value| value.as_object());

    if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
        for required_key in required.iter().filter_map(|value| value.as_str()) {
            if !input_object.contains_key(required_key) {
                return Err(format!("missing required field `{required_key}`"));
            }
        }
    }

    if schema
        .get("additionalProperties")
        .and_then(|value| value.as_bool())
        == Some(false)
    {
        if let Some(properties) = properties {
            for key in input_object.keys() {
                if !properties.contains_key(key) {
                    return Err(format!("unknown field `{key}`"));
                }
            }
        }
    }

    if let Some(properties) = properties {
        for (key, value) in input_object {
            if let Some(property_schema) = properties.get(key) {
                validate_json_schema_property(key, value, property_schema)?;
            }
        }
    }

    Ok(())
}

fn validate_json_schema_property(
    key: &str,
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    validate_json_schema_combinators(key, value, schema)?;

    if let Some(expected) = schema.get("const") {
        if expected != value {
            return Err(format!("field `{key}` must equal the required constant"));
        }
    }
    if let Some(values) = schema.get("enum").and_then(|value| value.as_array()) {
        if !values.iter().any(|candidate| candidate == value) {
            return Err(format!("field `{key}` must be one of the allowed values"));
        }
    }

    if let Some(kind) = schema.get("type") {
        if let Some(kinds) = kind.as_array() {
            if !kinds
                .iter()
                .filter_map(|value| value.as_str())
                .any(|kind| json_value_matches_schema_type(value, kind))
            {
                return Err(format!("field `{key}` has invalid type"));
            }
        } else if let Some(kind) = kind.as_str() {
            if !json_value_matches_schema_type(value, kind) {
                return Err(format!("field `{key}` must be {kind}"));
            }
        }
    }

    validate_json_schema_constraints(key, value, schema)
}

fn validate_json_schema_combinators(
    key: &str,
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    if let Some(all_of) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        for branch in all_of {
            validate_json_schema_property(key, value, branch)?;
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(serde_json::Value::as_array) {
        if !any_of
            .iter()
            .any(|branch| validate_json_schema_property(key, value, branch).is_ok())
        {
            return Err(format!("field `{key}` does not match any allowed schema"));
        }
    }
    if let Some(one_of) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
        let matches = one_of
            .iter()
            .filter(|branch| validate_json_schema_property(key, value, branch).is_ok())
            .count();
        if matches != 1 {
            return Err(format!(
                "field `{key}` must match exactly one allowed schema"
            ));
        }
    }
    if schema
        .get("not")
        .is_some_and(|branch| validate_json_schema_property(key, value, branch).is_ok())
    {
        return Err(format!("field `{key}` matches a forbidden schema"));
    }
    if let Some(condition) = schema.get("if") {
        let branch = if validate_json_schema_property(key, value, condition).is_ok() {
            schema.get("then")
        } else {
            schema.get("else")
        };
        if let Some(branch) = branch {
            validate_json_schema_property(key, value, branch)?;
        }
    }
    Ok(())
}

fn validate_json_schema_constraints(
    key: &str,
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> Result<(), String> {
    if let Some(string) = value.as_str() {
        if let Some(min_length) = schema.get("minLength").and_then(|value| value.as_u64()) {
            if (string.chars().count() as u64) < min_length {
                return Err(format!(
                    "field `{key}` must be at least {min_length} character(s)"
                ));
            }
        }
        if let Some(max_length) = schema.get("maxLength").and_then(|value| value.as_u64()) {
            if (string.chars().count() as u64) > max_length {
                return Err(format!(
                    "field `{key}` must be at most {max_length} character(s)"
                ));
            }
        }
        if let Some(pattern) = schema.get("pattern").and_then(serde_json::Value::as_str) {
            let pattern = regex::Regex::new(pattern)
                .map_err(|error| format!("field `{key}` has invalid schema pattern: {error}"))?;
            if !pattern.is_match(string) {
                return Err(format!("field `{key}` does not match the required pattern"));
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if schema
            .get("minimum")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|minimum| number < minimum)
        {
            return Err(format!("field `{key}` is below the minimum"));
        }
        if schema
            .get("maximum")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|maximum| number > maximum)
        {
            return Err(format!("field `{key}` exceeds the maximum"));
        }
        if schema
            .get("exclusiveMinimum")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|minimum| number <= minimum)
        {
            return Err(format!("field `{key}` must exceed the exclusive minimum"));
        }
        if schema
            .get("exclusiveMaximum")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|maximum| number >= maximum)
        {
            return Err(format!("field `{key}` must be below the exclusive maximum"));
        }
        if let Some(multiple) = schema
            .get("multipleOf")
            .and_then(serde_json::Value::as_f64)
            .filter(|multiple| *multiple > 0.0)
        {
            let quotient = number / multiple;
            if (quotient - quotient.round()).abs() > f64::EPSILON * 16.0 {
                return Err(format!("field `{key}` must be a multiple of {multiple}"));
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(|value| value.as_u64()) {
            if array.len() < min_items as usize {
                return Err(format!(
                    "field `{key}` must contain at least {min_items} item(s)"
                ));
            }
        }
        if let Some(max_items) = schema.get("maxItems").and_then(|value| value.as_u64()) {
            if array.len() > max_items as usize {
                return Err(format!(
                    "field `{key}` must contain at most {max_items} item(s)"
                ));
            }
        }
        if schema
            .get("uniqueItems")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            for (index, item) in array.iter().enumerate() {
                if array[..index].iter().any(|previous| previous == item) {
                    return Err(format!("field `{key}` must contain unique items"));
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema_property(&format!("{key}[{index}]"), item, item_schema)?;
            }
        }
    }

    if let Some(object) = value.as_object() {
        validate_json_schema_object(key, object, schema)?;
    }

    Ok(())
}

fn validate_json_schema_object(
    key: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    schema: &serde_json::Value,
) -> Result<(), String> {
    let properties = schema.get("properties").and_then(|value| value.as_object());
    if schema
        .get("minProperties")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|minimum| object.len() < minimum as usize)
    {
        return Err(format!("field `{key}` has too few properties"));
    }
    if schema
        .get("maxProperties")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|maximum| object.len() > maximum as usize)
    {
        return Err(format!("field `{key}` has too many properties"));
    }
    if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
        for required_key in required.iter().filter_map(|value| value.as_str()) {
            if !object.contains_key(required_key) {
                return Err(format!(
                    "field `{key}` missing required field `{required_key}`"
                ));
            }
        }
    }

    let additional_properties = schema.get("additionalProperties");
    for (property_key, property_value) in object {
        if let Some(property_schema) =
            properties.and_then(|properties| properties.get(property_key))
        {
            validate_json_schema_property(
                &format!("{key}.{property_key}"),
                property_value,
                property_schema,
            )?;
        } else if additional_properties.and_then(|value| value.as_bool()) == Some(false) {
            return Err(format!("field `{key}` has unknown field `{property_key}`"));
        } else if let Some(additional_schema) =
            additional_properties.filter(|value| value.is_object())
        {
            validate_json_schema_property(
                &format!("{key}.{property_key}"),
                property_value,
                additional_schema,
            )?;
        }
    }
    Ok(())
}

fn json_value_matches_schema_type(value: &serde_json::Value, kind: &str) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Maps to: CC `recheckPermission` / session-rule re-evaluation before an
/// interactive prompt is shown. This lets `AlwaysAllow` decisions made earlier
/// in the same tool batch suppress later identical permission prompts while
/// keeping permission interpretation inside the tool execution boundary.
pub fn should_ask_permission_request(
    request: &PermissionRequest,
    context: &ToolUseContext,
) -> bool {
    !has_in_memory_allow_rule(&context.tool_permission_context, &request.rule)
}

/// Maps to the pre-permission `runPreToolUseHooks(...)` portion of CC
/// `checkPermissionsAndCallTool(...)`.
pub async fn prepare_permission_request_before_prompt(
    request: &PermissionRequest,
    context: &ToolUseContext,
) -> PreToolUsePrepareResult {
    match load_tool_hooks_config_and_env(context) {
        Some((config, base_env)) => {
            prepare_permission_request_before_prompt_with_config(
                request,
                context,
                Some(&config),
                base_env,
                Some(&context.abort_controller),
            )
            .await
        }
        None => {
            prepare_permission_request_before_prompt_with_config(
                request,
                context,
                None,
                Vec::new(),
                Some(&context.abort_controller),
            )
            .await
        }
    }
}

// ─── Tool-result message constructors ────────────────────────────────────
// Maps to: CC `services/tools/toolExecution.ts` — the orchestration layer
// that wraps a tool's output/error into the user-visible `tool_result`
// message (`runToolUse(...)` / `checkPermissionsAndCallTool(...)` result
// handling). Per-tool execution bodies live in `src/tools/<tool>/mod.rs`,
// matching CC per-tool `call()` ownership.

/// Parse the structured input carried by a permission request, falling back
/// to the display summary for legacy senders.
/// Maps to: CC `services/tools/toolExecution.ts` `checkPermissionsAndCallTool`
/// input parsing seam (`tool.inputSchema.safeParse(input)`, toolExecution.ts:615).
pub(crate) fn parse_request_input(request: &PermissionRequest) -> serde_json::Value {
    if !request.input.is_null() {
        if !matches!(
            &request.input,
            serde_json::Value::Object(map)
                if map.len() == 1 && map.contains_key("summary")
        ) {
            return request.input.clone();
        }
    }
    serde_json::from_str(&request.input_summary)
        .unwrap_or_else(|_| serde_json::json!({ "value": request.input_summary }))
}

/// Maps to CC `toolExecution.ts` hook-updated input and `callInput`
/// restoration after observable path backfill.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HookInputRevalidation {
    Passthrough,
    Accepted(serde_json::Value),
    Ask(serde_json::Value),
    Denied(serde_json::Value),
}

/// Existing typed Rust adapter for the already-ported Glob/Grep tools.
fn revalidate_glob_grep_hook_input(
    request: &PermissionRequest,
    updated_input: &serde_json::Value,
    context: &ToolUseContext,
) -> HookInputRevalidation {
    let Some(tool) = find_tool_call(&request.tool_name) else {
        return HookInputRevalidation::Passthrough;
    };
    if !matches!(tool.name(), "Glob" | "Grep") {
        return HookInputRevalidation::Accepted(updated_input.clone());
    }
    let normalized = tool.normalize_input_with_context(updated_input, context);
    let schema = context
        .tools
        .iter()
        .find(|definition| {
            crate::types::tools::tool_matches_name(definition, tool.name())
                || crate::types::tools::tool_matches_name(definition, &request.tool_name)
        })
        .cloned()
        .or_else(|| {
            crate::tools::get_tools(&context.tool_permission_context)
                .into_iter()
                .find(|definition| crate::types::tools::tool_matches_name(definition, tool.name()))
        });
    if schema.is_some_and(|definition| {
        validate_tool_input_for_execution(tool.name(), &normalized, &definition.input_schema)
            .is_err()
    }) {
        return HookInputRevalidation::Passthrough;
    }
    let observable = tool.backfill_observable_input(&normalized, context);
    if !matches!(
        tool.validate_input(&observable, context),
        crate::tool::ValidationResult::Ok
    ) {
        return HookInputRevalidation::Passthrough;
    }
    match tool.check_permissions(&observable, context) {
        crate::utils::permissions::permission_result::PermissionResult::Allow { .. }
        | crate::utils::permissions::permission_result::PermissionResult::Passthrough { .. } => {
            HookInputRevalidation::Accepted(observable)
        }
        crate::utils::permissions::permission_result::PermissionResult::Ask { .. } => {
            HookInputRevalidation::Ask(observable)
        }
        crate::utils::permissions::permission_result::PermissionResult::Deny { .. } => {
            HookInputRevalidation::Denied(observable)
        }
    }
}

/// Wrap a tool failure into the official error `tool_result` message shape.
/// Maps to: CC `services/tools/toolExecution.ts` tool-error result handling
/// (`classifyToolError`, toolExecution.ts:150, + error result emission).
/// Maps to CC `toolExecution.ts:1716-1726`: the catch path records
/// `Error: {content}` as the raw `toolUseResult` for every tool, the wrapper
/// tag stripped the way the throwing site reports it.
pub(crate) fn tool_error_result(request: &PermissionRequest, content: String) -> RenderableMessage {
    let error = crate::utils::messages::extract_tag(&content, "tool_use_error")
        .unwrap_or_else(|| content.clone());
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        request.tool_use_id.clone(),
        content,
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(format!("Error: {error}"))))
}

async fn dynamic_mcp_tool_result(
    request: &PermissionRequest,
    context: &crate::tool::ToolUseContext,
) -> Option<RenderableMessage> {
    // Maps to: CC dynamic MCP Tool.call created by
    // `services/mcp/client.ts#fetchToolsForClient`.
    let (server_name, tool_name) = crate::services::mcp::client::resolve_mcp_tool_invocation(
        &request.tool_name,
        &context.mcp_state,
    )?;
    let server = context
        .mcp_state
        .clients
        .iter()
        .find(|server| server.client.name == server_name);
    if tool_name == "authenticate"
        && server.map(|server| server.client.status)
            == Some(crate::services::mcp::types::McpServerConnectionType::NeedsAuth)
    {
        let Some(config) = server.and_then(|server| server.config.as_ref()).cloned() else {
            return Some(tool_error_result(
                request,
                format!(
                    "<tool_use_error>Error: MCP server configuration was not found for {server_name}</tool_use_error>"
                ),
            ));
        };
        let tool = crate::tools::mcp_auth_tool::create_mcp_auth_tool(&server_name, &config);
        let output = tool.call(context).await;
        return Some(tool.map_tool_result_to_tool_result_block_param(request, output));
    }
    let args = match parse_request_input(request) {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let meta = Some(crate::services::mcp::client::mcp_tool_use_id_meta(
        &request.tool_use_id,
    ));
    match crate::services::mcp::client::call_mcp_tool_with_elicitation(
        &server_name,
        &tool_name,
        args.clone(),
        meta,
        context.handle_elicitation.clone(),
    )
    .await
    {
        Ok(value) => {
            if value
                .get("isError")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                let error_details = value
                    .get("content")
                    .and_then(|content| content.as_array())
                    .and_then(|content| content.first())
                    .and_then(|first| first.get("text"))
                    .and_then(|text| text.as_str())
                    .map(str::to_string)
                    .or_else(|| value.get("error").map(ToString::to_string))
                    .unwrap_or_else(|| "Unknown error".to_string());
                return Some(tool_error_result(
                    request,
                    format!("<tool_use_error>Error: {error_details}</tool_use_error>"),
                ));
            }

            let transformed = match crate::services::mcp::client::process_mcp_result(
                &value,
                &tool_name,
                &server_name,
            )
            .await
            {
                Ok(transformed) => transformed,
                Err(error) => {
                    return Some(tool_error_result(
                        request,
                        format!("<tool_use_error>Error: {error}</tool_use_error>"),
                    ));
                }
            };
            let summary =
                crate::services::mcp::client::transformed_mcp_result_summary(&transformed);
            // No Mcp display shape — the raw content-blocks value rides
            // the row as `toolUseResult` (`data: mcpResult.content`,
            // client.ts:1898) and the by-tool-name dispatch renders it.
            Some(
                RenderableMessage::user_tool_result(
                    uuid::Uuid::new_v4().to_string(),
                    request.tool_use_id.clone(),
                    summary.clone(),
                    false,
                )
                .with_tool_use_result(Some(transformed.content)),
            )
        }
        Err(error) => Some(tool_error_result(
            request,
            format!("<tool_use_error>Error: {error}</tool_use_error>"),
        )),
    }
}

/// Normalize a path for display (forward slashes on all platforms).
pub(crate) fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Terminal (deny/cancel) `tool_result` with official control copy.
/// Maps to: CC `services/tools/toolExecution.ts` rejected/cancelled
/// tool-use result emission (REJECT_MESSAGE / CANCEL_MESSAGE flow).
pub(crate) fn permission_terminal_result(
    request: &PermissionRequest,
    status: ToolResultStatus,
    is_subagent: bool,
) -> RenderableMessage {
    permission_terminal_result_with_feedback(request, status, None, is_subagent)
}

/// Maps to: CC `PermissionContext.cancelAndAbort(feedback, ...)` reject-copy
/// shaping for permission dialogs that collect user instructions.
pub(crate) fn permission_terminal_result_with_feedback(
    request: &PermissionRequest,
    status: ToolResultStatus,
    feedback: Option<&str>,
    is_subagent: bool,
) -> RenderableMessage {
    permission_terminal_result_for_decision(request, status, feedback, None, is_subagent)
}

/// Maps to: CC `services/tools/toolExecution.ts:1023`
/// `let errorMessage = permissionDecision.message`.
///
/// `decision_message` is the resolving decision's own `message` when the
/// decision came from the permission system (a rule deny, a tool
/// `checkPermissions` deny, the auto-mode classifier). It is `None` when the
/// permission dialog resolved the request, because CC replaces the decision
/// there with `PermissionContext.cancelAndAbort`'s
/// `{ behavior: 'ask', message: REJECT_MESSAGE… }` (`PermissionContext.ts:154-172`)
/// — which is exactly the copy the `Rejected` arms below produce.
///
/// `is_subagent` is CC's `const sub = !!toolUseContext.agentId`
/// (`PermissionContext.ts:159`). It selects the SUBAGENT_* copy and suppresses
/// `withMemoryCorrectionHint`; the third thing `sub` decides — whether the
/// controller is aborted — belongs to
/// `hooks::tool_permission::permission_context::cancel_and_abort`, which the
/// dialog seam calls.
pub(crate) fn permission_terminal_result_for_decision(
    request: &PermissionRequest,
    status: ToolResultStatus,
    feedback: Option<&str>,
    decision_message: Option<&str>,
    is_subagent: bool,
) -> RenderableMessage {
    use crate::hooks::tool_permission::permission_context::cancel_and_abort_message;

    let trimmed_feedback = feedback.map(str::trim).filter(|value| !value.is_empty());
    let decision_message = decision_message.filter(|message| !message.is_empty());
    let content = match (status, request.tool_name.as_str(), trimmed_feedback) {
        (ToolResultStatus::Rejected, _, None) if decision_message.is_some() => {
            decision_message.unwrap_or_default().to_string()
        }
        // No AskUserQuestion special case: CC's "User declined to answer
        // questions" lives ONLY in the UI renderer
        // (`AskUserQuestionTool.tsx:285-292` renderToolUseRejectedMessage);
        // the MODEL receives the generic reject copy like every other tool.
        // Writing the UI string here also broke `derived_status()` — the
        // sentinel is what marks the result Rejected for the renderer.
        (ToolResultStatus::Rejected, _, feedback) => {
            cancel_and_abort_message(is_subagent, feedback)
        }
        // CC `toolExecution.ts:443-444`: `content.content =
        // withMemoryCorrectionHint(CANCEL_MESSAGE)` while `toolUseResult` keeps
        // the bare sentinel — the `raw` below preserves that split.
        (ToolResultStatus::Canceled, _, _) => crate::utils::messages::with_memory_correction_hint(
            crate::utils::messages::CANCEL_MESSAGE,
        ),
        _ => String::new(),
    };
    // Every denied tool records `Error: {message}` as its raw `toolUseResult`
    // (CC `toolExecution.ts:1068`, which reuses the same hinted `errorMessage`),
    // while a cancelled one records the bare un-hinted sentinel with no prefix
    // (`:448` `toolUseResult: CANCEL_MESSAGE`).
    let raw = match status {
        ToolResultStatus::Rejected => Some(serde_json::Value::String(format!("Error: {content}"))),
        ToolResultStatus::Canceled => Some(serde_json::Value::String(
            crate::utils::messages::CANCEL_MESSAGE.to_string(),
        )),
        _ => None,
    };
    // CC wire shape (query.ts:136-147): reject and cancel both travel as
    // `is_error: true`; the sentinel text in `content` distinguishes them.
    RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        request.tool_use_id.clone(),
        content,
        !matches!(status, ToolResultStatus::Success),
    )
    .with_tool_use_result(raw)
}

/// Maps to: CC `services/tools/toolExecution.ts#checkPermissionsAndCallTool` pre-permission
/// hook segment; extracted only for Rust's documented deferred-permission boundary.
pub async fn prepare_permission_request_before_prompt_with_config(
    request: &PermissionRequest,
    context: &ToolUseContext,
    hooks_config: Option<&RegisteredHooks>,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> PreToolUsePrepareResult {
    let Some(config) = hooks_config else {
        return PreToolUsePrepareResult {
            request: request.clone(),
            hook_messages: Vec::new(),
            hook_model_messages: Vec::new(),
            forced_choice: None,
            force_ask: false,
            prevent_continuation: false,
            stop_reason: None,
            permission_updates: Vec::new(),
            hook_supplied_updated_input: false,
        };
    };
    let hook_result = run_pre_tool_use_hooks(
        request,
        config,
        &tool_hook_context(context),
        base_env,
        abort_controller,
    )
    .await;
    let hook_model_messages = hook_result.model_messages.clone();
    let hook_messages = hook_result.messages;
    let mut prepared_request = request.clone();
    // CC toolExecution.ts:831-837 retains the full hookPermissionResult;
    // toolHooks.ts:412-437 passes deny through or uses ask as forceDecision.
    if let Some(message) = hook_result.permission_message.as_ref() {
        prepared_request.message = message.clone();
    }
    if let Some(reason) = hook_result.permission_decision_reason.as_ref() {
        prepared_request.decision_reason = Some(reason.clone());
        prepared_request.is_compound_command = false;
    }
    let mut reroute_requires_ask = false;
    let mut reroute_denied = false;
    if let Some(updated_input) = hook_result.updated_input.as_ref() {
        let adapted_input = if prepared_request.tool_name == "Read" {
            HookInputRevalidation::Accepted(updated_input.clone())
        } else {
            revalidate_glob_grep_hook_input(&prepared_request, updated_input, context)
        };
        match adapted_input {
            HookInputRevalidation::Passthrough => {}
            HookInputRevalidation::Accepted(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
            }
            HookInputRevalidation::Ask(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
                reroute_requires_ask = true;
            }
            HookInputRevalidation::Denied(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
                reroute_denied = true;
            }
        }
    }
    let mut forced_choice = None;
    let mut force_ask = false;
    let permission_updates = Vec::new();
    if hook_result.stop_execution
        || matches!(
            hook_result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Deny)
        )
    {
        forced_choice = Some(PermissionPromptChoice::Deny);
    } else if matches!(
        hook_result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Allow)
    ) {
        forced_choice = Some(PermissionPromptChoice::AllowOnce);
    } else if matches!(
        hook_result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Ask)
    ) {
        force_ask = true;
        // CC toolHooks.ts:529-546: this is the actual hook ask object, which
        // resolveHookPermissionDecision later supplies as forceDecision.
        prepared_request.permission_result = Some(PermissionDecision::Ask {
            message: prepared_request.message.clone(),
            updated_input: hook_result.updated_input.clone(),
            decision_reason: prepared_request.decision_reason.clone(),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        });
    }
    if reroute_denied {
        forced_choice = Some(PermissionPromptChoice::Deny);
        force_ask = false;
    } else if reroute_requires_ask {
        force_ask = true;
    }

    PreToolUsePrepareResult {
        request: prepared_request,
        hook_messages,
        hook_model_messages,
        forced_choice,
        force_ask,
        prevent_continuation: hook_result.prevent_continuation,
        stop_reason: hook_result.stop_reason,
        permission_updates,
        // CC attaches `updatedInput` to the hook ALLOW decision only
        // (`toolHooks.ts:520-527`); the passthrough updatedInput case has no
        // permission decision and does not satisfy the interaction.
        hook_supplied_updated_input: matches!(
            hook_result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Allow)
        ) && hook_result.updated_input.is_some(),
    }
}

async fn prepare_permission_prompt_hooks_with_config(
    request: &PermissionRequest,
    context: &ToolUseContext,
    config: Option<&RegisteredHooks>,
    base_env: Vec<(String, String)>,
    abort_controller: Option<&crate::tool::AbortController>,
) -> PreToolUsePrepareResult {
    let Some(config) = config else {
        return PreToolUsePrepareResult {
            request: request.clone(),
            hook_messages: Vec::new(),
            hook_model_messages: Vec::new(),
            forced_choice: None,
            force_ask: false,
            prevent_continuation: false,
            stop_reason: None,
            permission_updates: Vec::new(),
            hook_supplied_updated_input: false,
        };
    };
    let result = run_permission_request_hooks_with_config(
        request,
        config,
        &tool_hook_context(context),
        base_env,
        abort_controller,
    )
    .await;
    let mut prepared_request = request.clone();
    if matches!(
        result.permission_request_result,
        Some(crate::types::hooks::PermissionRequestResult::Allow { .. })
    ) {
        // Maps to CC PermissionContext.ts:139-146#persistPermissions and
        // :319-335#handleHookAllow. L1 effect split at the existing retained
        // permission boundary: persist here before resolving
        // the hook; the query/streaming caller applies these same updates to
        // its live context. Reuse the canonical writer, including its caught
        // ordinary disk failures, rather than manufacturing a second policy.
        if let Err(error) = crate::utils::permissions::permission_update::persist_permission_updates(
            &result.permission_updates,
        ) {
            // L2 retained COMETIX_WRITE_ENABLED=0 policy: the canonical entry's
            // only propagated error blocks this grant without disk/live updates.
            // Carry a system deny message so it cannot become a user cancel.
            let message = error.to_string();
            crate::utils::debug::log_for_debugging(&message);
            prepared_request.message = message.clone();
            prepared_request.decision_reason =
                Some(crate::types::permissions::PermissionDecisionReason::Hook {
                    hook_name: "PermissionRequest".to_string(),
                    hook_source: None,
                    reason: Some(message),
                });
            return PreToolUsePrepareResult {
                request: prepared_request,
                hook_messages: result.messages,
                hook_model_messages: result.model_messages,
                forced_choice: Some(PermissionPromptChoice::Deny),
                force_ask: false,
                prevent_continuation: false,
                stop_reason: None,
                permission_updates: Vec::new(),
                hook_supplied_updated_input: false,
            };
        }
        // buildAllow has no message and replaces the earlier Ask's reason.
        prepared_request.message.clear();
        prepared_request.decision_reason =
            Some(crate::types::permissions::PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: None,
            });
        prepared_request.is_compound_command = false;
    }
    // Maps to CC hooks/toolPermission/PermissionContext.ts:252-260. The
    // resolving deny supplies the model-facing message and hook reason;
    // an empty reason remains empty even though the message uses a fallback.
    if let Some(crate::types::hooks::PermissionRequestResult::Deny { message, .. }) =
        result.permission_request_result.as_ref()
    {
        prepared_request.message = message
            .clone()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Permission denied by hook".to_string());
        prepared_request.decision_reason =
            Some(crate::types::permissions::PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: message.clone(),
            });
    }
    let mut reroute_requires_ask = false;
    let mut reroute_denied = false;
    if let Some(updated_input) = result.updated_input.as_ref() {
        let adapted_input = if prepared_request.tool_name == "Read" {
            HookInputRevalidation::Accepted(updated_input.clone())
        } else {
            revalidate_glob_grep_hook_input(&prepared_request, updated_input, context)
        };
        match adapted_input {
            HookInputRevalidation::Passthrough => {}
            HookInputRevalidation::Accepted(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
            }
            HookInputRevalidation::Ask(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
                reroute_requires_ask = true;
            }
            HookInputRevalidation::Denied(updated_input) => {
                prepared_request =
                    permission_request_with_updated_input(prepared_request, updated_input);
                reroute_denied = true;
            }
        }
    }
    let mut forced_choice = if result.prevent_continuation
        || matches!(
            result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Deny)
        ) {
        Some(PermissionPromptChoice::Deny)
    } else if matches!(
        result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Allow)
    ) {
        Some(PermissionPromptChoice::AllowOnce)
    } else {
        None
    };
    let mut force_ask = matches!(
        result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Ask)
    );
    if reroute_denied {
        forced_choice = Some(PermissionPromptChoice::Deny);
        force_ask = false;
    } else if reroute_requires_ask {
        force_ask = true;
    }
    if matches!(forced_choice, Some(PermissionPromptChoice::AllowOnce)) {
        // CC PermissionContext.ts:233-239,334 → toolExecution.ts:1129-1133:
        // the resolving permission input is authoritative for tool.call.
        // Update the existing Rust mutation-destination transport after that
        // final input is installed; its same-logical-path guard preserves a
        // prior physical pin rather than approving a symlink retarget.
        pin_user_approved_edit_destination(&mut prepared_request, context);
    }
    PreToolUsePrepareResult {
        request: prepared_request,
        hook_messages: result.messages,
        hook_model_messages: result.model_messages,
        forced_choice,
        force_ask,
        prevent_continuation: false,
        stop_reason: None,
        permission_updates: result.permission_updates,
        // Same contract as the PreToolUse producer: only an ALLOW decision
        // that carries `updatedInput` satisfies the interaction.
        hook_supplied_updated_input: matches!(
            result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Allow)
        ) && result.updated_input.is_some(),
    }
}

pub async fn prepare_permission_prompt_hooks(
    request: &PermissionRequest,
    context: &ToolUseContext,
) -> PreToolUsePrepareResult {
    // CC has NO prompt-boundary PermissionRequest seam in non-interactive
    // mode: `getCanUseToolFn` (`cli/print.ts:4267-4293`) resolves to either
    // `structuredIO.createCanUseTool` — whose PermissionRequest hooks race
    // the SDK prompt INSIDE the callback (`cli/structuredIO.ts:561-657`,
    // ported at `cli/print.rs#request_sdk_tool_permission`) — or, with no
    // permission prompt tool, a bare `hasPermissionsToUseTool` whose only
    // PermissionRequest hook owner is the headless ask tail
    // (`permissions.ts:929-952`, ported at
    // `utils/permissions/permissions.rs#resolve_headless_ask_owned`).
    // Running this interactive-dialog approximation here as well executed
    // the same hooks a second time ahead of the SDK race (issue-3 High 2).
    if context.is_non_interactive_session {
        return PreToolUsePrepareResult {
            request: request.clone(),
            hook_messages: Vec::new(),
            hook_model_messages: Vec::new(),
            forced_choice: None,
            force_ask: false,
            prevent_continuation: false,
            stop_reason: None,
            permission_updates: Vec::new(),
            hook_supplied_updated_input: false,
        };
    }
    match load_tool_hooks_config_and_env(context) {
        Some((config, base_env)) => {
            prepare_permission_prompt_hooks_with_config(
                request,
                context,
                Some(&config),
                base_env,
                Some(&context.abort_controller),
            )
            .await
        }
        None => {
            prepare_permission_prompt_hooks_with_config(
                request,
                context,
                None,
                Vec::new(),
                Some(&context.abort_controller),
            )
            .await
        }
    }
}

/// Maps to: CC `streamedCheckPermissionsAndCallTool(...)`.
///
/// Streaming progress and hook yields are still TODO; this function preserves
/// the official control-flow position and delegates to the non-streaming
/// permission/tool-result seam for now.
pub async fn streamed_check_permissions_and_call_tool(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    match load_tool_hooks_config_and_env(context) {
        Some((config, base_env)) => {
            streamed_check_permissions_and_call_tool_with_config(
                request,
                choice,
                context,
                assistant_message,
                Some(&config),
                base_env,
            )
            .await
        }
        None => {
            streamed_check_permissions_and_call_tool_with_config(
                request,
                choice,
                context,
                assistant_message,
                None,
                Vec::new(),
            )
            .await
        }
    }
}

pub async fn streamed_check_permissions_and_call_tool_after_pre_tool_hooks(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
        request,
        &PermissionPromptResponse::new(choice),
        false,
        None,
        context,
        assistant_message,
    )
    .await
}

pub async fn streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
    prevent_continuation: bool,
    stop_reason: Option<&str>,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    match load_tool_hooks_config_and_env(context) {
        Some((config, base_env)) => {
            check_permissions_and_call_tool_with_config_inner(
                request,
                response.clone(),
                context,
                assistant_message,
                &config,
                base_env,
                false,
                prevent_continuation,
                stop_reason,
            )
            .await
        }
        None => {
            check_permissions_and_call_tool_with_response_async(
                request,
                response,
                prevent_continuation,
                stop_reason,
                context,
                assistant_message,
            )
            .await
        }
    }
}

pub async fn streamed_check_permissions_and_call_tool_with_config(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
    hooks_config: Option<&RegisteredHooks>,
    base_env: Vec<(String, String)>,
) -> ToolExecutionResult {
    if let Some(config) = hooks_config {
        check_permissions_and_call_tool_with_config(
            request,
            choice,
            context,
            assistant_message,
            config,
            base_env,
        )
        .await
    } else {
        check_permissions_and_call_tool_async(request, choice, context, assistant_message).await
    }
}

/// Maps to: CC `checkPermissionsAndCallTool(...)`.
///
/// Applies the interactive permission choice to `ToolPermissionContext`, then
/// asks the executor layer to produce the user-visible `tool_result`. Query code
/// must not create these messages directly.
pub fn check_permissions_and_call_tool(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    futures::executor::block_on(check_permissions_and_call_tool_async(
        request,
        choice,
        context,
        assistant_message,
    ))
}

pub fn check_permissions_and_call_tool_with_response(
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    futures::executor::block_on(check_permissions_and_call_tool_with_response_async(
        request,
        response,
        false,
        None,
        context,
        assistant_message,
    ))
}

/// Async implementation of CC `checkPermissionsAndCallTool(...)` used by the
/// query actor path. The sync wrapper above remains for legacy/tests, while
/// production `streamedCheckPermissionsAndCallTool(...)` awaits `Tool.call(...)`
/// instead of nesting a block-on inside the actor loop.
pub async fn check_permissions_and_call_tool_async(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    check_permissions_and_call_tool_with_response_async(
        request,
        &PermissionPromptResponse::new(choice),
        false,
        None,
        context,
        assistant_message,
    )
    .await
}

/// Maps to CC `services/tools/toolExecution.ts#addToolResult` appending
/// `permissionDecision.acceptFeedback` as a text content block after the
/// primary `tool_result` block.
fn append_accept_feedback_to_tool_result(
    mut tool_result: Option<UserMessage>,
    response: &PermissionPromptResponse,
    choice: PermissionPromptChoice,
) -> Option<UserMessage> {
    if !matches!(
        choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    ) {
        return tool_result;
    }
    let Some(feedback) = response
        .feedback
        .as_deref()
        .map(str::trim)
        .filter(|feedback| !feedback.is_empty())
    else {
        return tool_result;
    };
    if let Some(message) = tool_result.as_mut() {
        message
            .content
            .push(UserContent::Text(feedback.to_string()));
    }
    tool_result
}

/// Maps to: CC `services/tools/toolExecution.ts:1029-1068,1417-1438`.
fn append_permission_content_blocks(
    tool_result: &mut Option<UserMessage>,
    response: &PermissionPromptResponse,
) {
    let Some(message) = tool_result.as_mut() else {
        return;
    };
    message
        .content
        .extend(response.content_blocks.iter().map(|block| match block {
            crate::types::permissions::PermissionContentBlock::Text { text } => {
                UserContent::Text(text.clone())
            }
            crate::types::permissions::PermissionContentBlock::Image { source } => {
                UserContent::Image {
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                }
            }
        }));
}

/// Pins Edit and NotebookEdit destinations in private call transport. The
/// historical name is retained because all permission continuations already
/// converge on this helper.
fn pin_user_approved_edit_destination(request: &mut PermissionRequest, context: &ToolUseContext) {
    let (approved_key, destination_key, expanded, destination) =
        if request.tool_name == crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME {
            let Some(path) = request
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str)
            else {
                return;
            };
            let expanded = crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                .unwrap_or_else(|_| std::path::PathBuf::from(path));
            let destination = crate::tools::file_edit_tool::resolved_edit_destination(&expanded);
            (
                crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY,
                crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY,
                expanded,
                destination,
            )
        } else if request.tool_name
            == crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME
        {
            let Some(path) = request
                .input
                .get("notebook_path")
                .and_then(serde_json::Value::as_str)
            else {
                return;
            };
            let expanded = crate::utils::path::expand_path(path, Some(&context.effective_cwd()))
                .unwrap_or_else(|_| std::path::PathBuf::from(path));
            let destination =
                crate::tools::notebook_edit_tool::resolved_notebook_destination(&expanded);
            (
                crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_APPROVED_PATH_KEY,
                crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY,
                expanded,
                destination,
            )
        } else {
            return;
        };
    let call_input = request
        .call_input
        .get_or_insert_with(|| request.input.clone());
    if let Some(object) = call_input.as_object_mut() {
        let same_logical_approval = object
            .get(approved_key)
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from)
            .is_some_and(|approved| approved == expanded);
        let already_pinned = object
            .get(destination_key)
            .and_then(serde_json::Value::as_str)
            .is_some();
        object.insert(
            approved_key.to_string(),
            serde_json::Value::String(expanded.display().to_string()),
        );
        // Re-pin only when an authoritative hook/user update changed the
        // logical path. Re-running a continuation for the same approved path
        // must not bless a symlink retarget that happened while waiting.
        if !same_logical_approval || !already_pinned {
            object.insert(
                destination_key.to_string(),
                serde_json::Value::String(destination.display().to_string()),
            );
        }
    }
}

fn request_missing_checked_mutation_destination(request: &PermissionRequest) -> bool {
    let Some(call_input) = request.call_input.as_ref() else {
        return true;
    };
    let key = match request.tool_name.as_str() {
        crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME => {
            crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY
        }
        crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME => {
            crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY
        }
        _ => return false,
    };
    call_input.get(key).is_none()
}

fn edit_preflight_error_result(
    request: &PermissionRequest,
    context: &ToolUseContext,
    choice: PermissionPromptChoice,
    content: String,
) -> ToolExecutionResult {
    // CC's internal `toolUseResult: "Error: ..."` string rides the
    // row itself; the row carries no display shape.
    let message = RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        request.tool_use_id.clone(),
        content.clone(),
        true,
    )
    .with_tool_use_result(Some(serde_json::Value::String(format!("Error: {content}"))));
    let tool_result = transcript_tool_result_to_model_message(
        &message,
        crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME,
    );
    ToolExecutionResult {
        message: Some(message),
        hook_messages: Vec::new(),
        pre_tool_messages: Vec::new(),
        pre_tool_model_messages: Vec::new(),
        post_tool_messages: Vec::new(),
        post_tool_model_messages: Vec::new(),
        new_messages: Vec::new(),
        continuation_messages: Vec::new(),
        new_context: context.clone(),
        permission_decision: crate::utils::permissions::permissions::decision_for_choice(
            request, choice,
        ),
        tool_result,
        // Preflight refusal: `tool.call(...)` never ran, so there is no
        // `ToolResult.contextModifier` to read.
        context_modifier: None,
    }
}

fn mutation_disabled_result(
    request: &PermissionRequest,
    context: &ToolUseContext,
    choice: PermissionPromptChoice,
) -> Option<ToolExecutionResult> {
    if crate::utils::env_utils::is_cometix_write_enabled() {
        return None;
    }
    if request.tool_name == crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME {
        return Some(edit_preflight_error_result(
            request,
            context,
            choice,
            crate::tools::shared::write_gate::FILE_EDIT_DISABLED_ERROR.to_string(),
        ));
    }
    if request.tool_name != crate::tools::notebook_edit_tool::constants::NOTEBOOK_EDIT_TOOL_NAME {
        return None;
    }

    let error = crate::tools::shared::write_gate::NOTEBOOK_EDIT_DISABLED_ERROR;
    let output = crate::tools::notebook_edit_tool::notebook_edit_error_from_args(
        &request.input,
        &context.effective_cwd(),
        error,
    );
    // The error-as-data object rides the row as the raw `toolUseResult`;
    // the display records only the emit-gate decision.
    let message = RenderableMessage::user_tool_result(
        uuid::Uuid::new_v4().to_string(),
        request.tool_use_id.clone(),
        error,
        true,
    )
    .with_tool_use_result(Some(crate::tools::notebook_edit_tool::ui::output_to_value(
        &output,
    )));
    let tool_result = transcript_tool_result_to_model_message(&message, &request.tool_name);
    Some(ToolExecutionResult {
        message: Some(message),
        hook_messages: Vec::new(),
        pre_tool_messages: Vec::new(),
        pre_tool_model_messages: Vec::new(),
        post_tool_messages: Vec::new(),
        post_tool_model_messages: Vec::new(),
        new_messages: Vec::new(),
        continuation_messages: Vec::new(),
        new_context: context.clone(),
        permission_decision: crate::utils::permissions::permissions::decision_for_choice(
            request, choice,
        ),
        tool_result,
        // Same as above: the write gate refuses before `tool.call(...)`.
        context_modifier: None,
    })
}

fn permission_response_user_modified(
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
    context: &ToolUseContext,
) -> Result<bool, String> {
    if !matches!(
        response.choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    ) {
        return Ok(false);
    }
    let Some(updated_input) = response.updated_input.as_ref() else {
        return Ok(false);
    };
    Ok(find_tool_call(&request.tool_name)
        .map(|tool| tool.inputs_equivalent(&request.input, updated_input, context))
        .transpose()?
        .flatten()
        .is_some_and(|equivalent| !equivalent))
}

/// Maps to: CC `services/tools/toolExecution.ts:979-993` — "Add message if
/// permission was granted/denied by PermissionRequest hook":
///
/// ```text
/// if (
///   permissionDecision.decisionReason?.type === 'hook' &&
///   permissionDecision.decisionReason.hookName === 'PermissionRequest' &&
///   permissionDecision.behavior !== 'ask'
/// ) {
///   resultingMessages.push({
///     message: createAttachmentMessage({
///       type: 'hook_permission_decision',
///       decision: permissionDecision.behavior,
///       toolUseID,
///       hookEvent: 'PermissionRequest',
///     }),
///   })
/// }
/// ```
///
/// Only the PermissionRequest hook qualifies. A PreToolUse hook decision
/// carries `hookName: 'PreToolUse:<tool>'` (`toolHooks.ts:492-494,512-518`) and
/// is filtered out by the same `hookName` test CC applies. The three CC sites
/// that DO produce `{type:'hook', hookName:'PermissionRequest'}` —
/// `permissions.ts:436-459` (headless agent), `PermissionContext.ts:252-258`
/// and `:334` (interactive), `structuredIO.ts:834-853` (SDK) — are ported at
/// `utils/permissions/permissions.rs:1323`/`:1364` and `cli/print.rs:1495`/
/// `:1513`.
///
/// CC's one `permissionDecision` object is split across two carriers here: a
/// SYSTEM decision leaves its reason on [`PermissionRequest::decision_reason`],
/// while the decision that RESOLVED `canUseTool` leaves it on
/// [`PermissionPromptResponse::decision_reason`] (#170). The resolving decision
/// wins, which is what reading one object would have produced.
///
/// `hookEvent` is CC's literal `'PermissionRequest'`, not
/// `decisionReason.hookName` — the guard above already pins them equal.
fn hook_permission_decision_attachment(
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
    behavior: crate::types::permissions::PermissionBehavior,
) -> Option<AttachmentMessage> {
    use crate::types::permissions::{PermissionBehavior, PermissionDecisionReason};

    let decision_reason = response
        .decision_reason
        .as_ref()
        .or(request.decision_reason.as_ref())?;
    let PermissionDecisionReason::Hook { hook_name, .. } = decision_reason else {
        return None;
    };
    if hook_name != "PermissionRequest" {
        return None;
    }
    // CC `permissionDecision.behavior !== 'ask'`. `decision_for_choice` never
    // yields an ask, so this arm is CC's guard kept for shape.
    let decision = match behavior {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Deny => "deny",
        PermissionBehavior::Ask => return None,
    };
    Some(AttachmentMessage::new(
        crate::types::message::Attachment::HookPermissionDecision {
            decision: decision.to_string(),
            tool_use_id: request.tool_use_id.clone(),
            hook_event: "PermissionRequest".to_string(),
        },
    ))
}

/// Splices CC's single `resultingMessages.push({ message })` into this port's
/// two transports: the rendered row and the history/model message. Same split
/// as `utils/attachments.rs#attachment_continuation` — one attachment, one
/// uuid, both halves.
///
/// The pre-tool vectors are the "before the primary tool_result" slice of CC's
/// `resultingMessages`, which is exactly where `:985` pushes (ahead of the deny
/// result at `:1064` and the allow result at `:1456`).
///
/// No audience gate here, matching CC — the row renders on every build. The
/// TRANSCRIPT half still passes the ordinary attachment-writer gate
/// (`utils/session_storage.rs#typed_message_entry`, CC `isLoggableMessage`,
/// `sessionStorage.ts:4351-4367`): the model message reaches the JSONL through
/// `record_typed_messages`, which withholds attachments from external
/// transcripts. The render half is a `HistoryEntry::Row` and is never recorded,
/// so the user-visible row is audience-independent.
fn push_hook_permission_decision_attachment(
    attachment: AttachmentMessage,
    rows: &mut Vec<RenderableMessage>,
    model_messages: &mut Vec<Message>,
) {
    rows.push(RenderableMessage {
        uuid: attachment.uuid.clone(),
        kind: RenderableMessageKind::Attachment(attachment.attachment.clone()),
    });
    model_messages.push(Message::Attachment(attachment));
}

/// Maps to: CC `services/tools/toolExecution.ts#checkPermissionsAndCallTool` permission-response
/// continuation; Rust resumes this segment after the deferred permission actor responds.
pub async fn check_permissions_and_call_tool_with_response_async(
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
    prevent_continuation: bool,
    stop_reason: Option<&str>,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
) -> ToolExecutionResult {
    // Maps to: CC `runToolUse` abort gate ahead of `canUseTool`
    // (toolExecution.ts:415-453) — an already-aborted turn yields the
    // official cancelled tool_result instead of running hooks or the tool.
    if context.abort_controller.is_aborted() {
        let message = permission_terminal_result(
            request,
            crate::types::message::ToolResultStatus::Canceled,
            context.agent_id.is_some(),
        );
        let mut tool_result = transcript_tool_result_to_model_message(&message, &request.tool_name);
        // CC `toolExecution.ts:449` (pre-canUseTool abort cancel result).
        stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
        return ToolExecutionResult {
            message: Some(message),
            hook_messages: Vec::new(),
            pre_tool_messages: Vec::new(),
            pre_tool_model_messages: Vec::new(),
            post_tool_messages: Vec::new(),
            post_tool_model_messages: Vec::new(),
            new_messages: Vec::new(),
            continuation_messages: Vec::new(),
            new_context: context.clone(),
            // CC's pre-canUseTool abort gate (`toolExecution.ts:415-453`)
            // yields CANCEL_MESSAGE and returns WITHOUT any permission
            // decision. This field is Rust structural glue with no production
            // reader; the real cancellation signal is the Canceled-status
            // message above.
            permission_decision: crate::utils::permissions::permissions::decision_for_choice(
                request,
                PermissionPromptChoice::Deny,
            ),
            tool_result,
            // Aborted before `tool.call(...)`; nothing produced a modifier.
            context_modifier: None,
        };
    }

    if let Some(result) = mutation_disabled_result(request, context, response.choice) {
        return result;
    }

    // Maps to CC PermissionContext.handleUserAllow: semantic comparison is
    // performed against the user's prompt update before PreToolUse hooks can
    // rewrite the input for non-user reasons.
    let user_modified = match permission_response_user_modified(request, response, context) {
        Ok(user_modified) => user_modified,
        Err(error) => {
            return edit_preflight_error_result(request, context, response.choice, error);
        }
    };
    let mut effective_response = response.clone();
    let mut effective_request = effective_response.apply_to_request(request.clone());
    if response.updated_input.is_some() || request_missing_checked_mutation_destination(request) {
        pin_user_approved_edit_destination(&mut effective_request, context);
    }
    let mut hook_messages = Vec::new();
    // This compatibility path has no HooksConfig; configured actor execution
    // below calls the canonical async `run_pre_tool_use_hooks` owner.
    let pre_hook_result = ToolHookSeamResult::default();
    let mut pre_tool_model_messages = pre_hook_result.model_messages.clone();
    let mut pre_tool_messages = pre_hook_result.messages;
    hook_messages.extend(pre_tool_messages.clone());
    let mut hook_input_permission_blocked = false;
    if let Some(updated_input) = pre_hook_result.updated_input.as_ref() {
        let adapted_input = if effective_request.tool_name == "Read" {
            HookInputRevalidation::Accepted(updated_input.clone())
        } else {
            revalidate_glob_grep_hook_input(&effective_request, updated_input, context)
        };
        match adapted_input {
            HookInputRevalidation::Passthrough => {}
            HookInputRevalidation::Accepted(updated_input) => {
                effective_request =
                    permission_request_with_updated_input(effective_request, updated_input);
                effective_response.updated_input = Some(effective_request.input.clone());
            }
            HookInputRevalidation::Ask(updated_input)
            | HookInputRevalidation::Denied(updated_input) => {
                effective_request =
                    permission_request_with_updated_input(effective_request, updated_input);
                effective_response.updated_input = Some(effective_request.input.clone());
                hook_input_permission_blocked = true;
            }
        }
    }
    let should_prevent_continuation = prevent_continuation || pre_hook_result.prevent_continuation;
    let continuation_stop_reason = pre_hook_result
        .stop_reason
        .as_deref()
        .or(stop_reason)
        .map(str::to_string);
    if pre_hook_result.stop_execution
        || matches!(
            pre_hook_result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Deny)
        )
    {
        effective_response.choice = PermissionPromptChoice::Deny;
        effective_response.permission_updates.clear();
    } else if matches!(
        pre_hook_result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Allow)
    ) {
        effective_response.choice = PermissionPromptChoice::AllowOnce;
    }
    if hook_input_permission_blocked {
        effective_response.choice = PermissionPromptChoice::Deny;
        effective_response.permission_updates.clear();
    }

    let choice = effective_response.choice;
    let (next_permission_context, permission_decision) = apply_prompt_response(
        &context.tool_permission_context,
        &effective_request,
        &effective_response,
    );
    // CC `toolExecution.ts:979-993`, at its own position: after the permission
    // decision is final and before the `behavior !== 'allow'` branch, so the
    // row precedes both the deny result (`:1064`) and the allow one (`:1456`).
    if let Some(attachment) = hook_permission_decision_attachment(
        &effective_request,
        &effective_response,
        permission_decision.behavior,
    ) {
        push_hook_permission_decision_attachment(
            attachment,
            &mut pre_tool_messages,
            &mut pre_tool_model_messages,
        );
    }
    let mut new_context = context.clone();
    new_context.update_permission_context(next_permission_context);
    // Maps to CC `toolExecution.ts:1207-1213`: `tool.call(input,
    // {...toolUseContext, toolUseId: toolUseID, userModified})` — the per-call
    // context carries the executing tool_use block's id alongside userModified.
    new_context.user_modified = Some(user_modified);
    new_context.tool_use_id = Some(effective_request.tool_use_id.clone());
    apply_tool_context_effects_after_execution(&mut new_context, &effective_request, choice);

    let execution_result = LocalToolExecutor
        .check_permissions_and_call_tool(
            &effective_request,
            &effective_response,
            &new_context,
            assistant_message,
            None,
        )
        .await;
    let message = execution_result.message;
    let mut new_messages = execution_result.new_messages;
    let _hook_tool_response = execution_result.hook_tool_response.clone();
    let model_content_blocks = execution_result.model_content_blocks;
    // Maps to CC `toolExecution.ts:1465-1470`: pair the modifier read at
    // :1400 with the executing tool_use id.
    let context_modifier = execution_result.context_modifier.map(|operation| {
        ToolContextModifier::from_operation(effective_request.tool_use_id.clone(), operation)
    });
    if let Some(cwd) = execution_result.cwd_after_update {
        crate::utils::shell::apply_cwd_after_command(&mut new_context, cwd);
    }
    if let Some(output) = execution_result.structured_output_update {
        push_structured_output_attachment(&mut new_messages, &output);
        new_context.structured_output = Some(output);
    }
    append_inline_skill_attachments(&effective_request, &mut new_context, &mut new_messages).await;
    // This compatibility path has no HooksConfig; configured actor execution
    // below calls the canonical async run_* hook owners.
    let post_hook_result = ToolHookSeamResult::default();
    let post_tool_model_messages = post_hook_result.model_messages.clone();
    let post_tool_messages = post_hook_result.messages;
    hook_messages.extend(post_tool_messages.clone());

    let mut tool_result = append_accept_feedback_to_tool_result(
        message.as_ref().and_then(|message| {
            transcript_tool_result_to_model_message(message, &effective_request.tool_name)
        }),
        &effective_response,
        choice,
    );
    append_permission_content_blocks(&mut tool_result, &effective_response);
    attach_model_content_blocks(&mut tool_result, model_content_blocks);
    // CC `toolExecution.ts` result yields (:857/:1069/:1465/:1733): every
    // tool_result records the uuid of its tool_use's assistant message.
    stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
    let continuation_messages = if should_prevent_continuation
        && matches!(
            choice,
            PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
        )
        && matches!(
            tool_result_status(message.as_ref()),
            Some(ToolResultStatus::Success)
        ) {
        vec![Message::Attachment(AttachmentMessage::new(
            serde_json::json!({
                "type": "hook_stopped_continuation",
                "message": continuation_stop_reason
                    .unwrap_or_else(|| "Execution stopped by hook".to_string()),
                "hookName": format!("PreToolUse:{}", effective_request.tool_name),
                "toolUseID": effective_request.tool_use_id,
                "hookEvent": "PreToolUse",
            }),
        ))]
    } else {
        Vec::new()
    };

    apply_tool_context_modifier(&mut new_context, context_modifier.as_ref());

    ToolExecutionResult {
        message,
        hook_messages,
        pre_tool_messages,
        pre_tool_model_messages,
        post_tool_messages,
        post_tool_model_messages,
        new_messages,
        continuation_messages,
        new_context,
        permission_decision,
        tool_result,
        context_modifier,
    }
}

/// Maps to: CC `toolOrchestration.ts:140-142` (`runToolsSerially` consumer) and
/// `StreamingToolExecutor.ts:391-395` — the orchestration layer applies the
/// tool's `contextModifier` to the context threaded into every SUBSEQUENT tool
/// use of the query.
///
/// This port hands that context back through `ToolExecutionResult.new_context`
/// (`query.rs` adopts it after a resumed permission, the streaming worker after
/// each completion), so the application belongs at the tail of tool execution.
/// It runs AFTER PostToolUse hooks so the hook payload still observes the
/// unmodified context, matching CC — where the modifier only lands once the
/// orchestration loop consumes the yielded update.
fn apply_tool_context_modifier(
    new_context: &mut ToolUseContext,
    context_modifier: Option<&ToolContextModifier>,
) {
    let Some(modifier) = context_modifier else {
        return;
    };
    *new_context = modifier.modify_context(new_context.clone());
}

pub async fn check_permissions_and_call_tool_with_config(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
    hooks_config: &RegisteredHooks,
    base_env: Vec<(String, String)>,
) -> ToolExecutionResult {
    check_permissions_and_call_tool_with_config_inner(
        request,
        PermissionPromptResponse::new(choice),
        context,
        assistant_message,
        hooks_config,
        base_env,
        true,
        false,
        None,
    )
    .await
}

/// Maps to: CC `services/tools/toolExecution.ts#checkPermissionsAndCallTool`; the injected
/// `HooksConfig` is Rust's test/runtime configuration seam for the same canonical flow.
async fn check_permissions_and_call_tool_with_config_inner(
    request: &PermissionRequest,
    mut response: PermissionPromptResponse,
    context: &ToolUseContext,
    assistant_message: Option<&AssistantMessage>,
    hooks_config: &RegisteredHooks,
    base_env: Vec<(String, String)>,
    run_pre_hooks: bool,
    prepared_prevent_continuation: bool,
    prepared_stop_reason: Option<&str>,
) -> ToolExecutionResult {
    if let Some(result) = mutation_disabled_result(request, context, response.choice) {
        return result;
    }
    let user_modified = match permission_response_user_modified(request, &response, context) {
        Ok(user_modified) => user_modified,
        Err(error) => {
            return edit_preflight_error_result(request, context, response.choice, error);
        }
    };
    let mut user_approved_request = response.apply_to_request(request.clone());
    if response.updated_input.is_some() || request_missing_checked_mutation_destination(request) {
        pin_user_approved_edit_destination(&mut user_approved_request, context);
    }
    let mut hook_messages = Vec::new();
    let pre_hook_result = if run_pre_hooks {
        run_pre_tool_use_hooks(
            request,
            hooks_config,
            // CC `toolHooks.ts:464-471` reads `appState.toolPermissionContext
            // .mode` for PreToolUse at `toolExecution.ts:800` — BEFORE the
            // permission decision (`:921`). The post events re-read it after
            // (`toolHooks.ts:52-53`, `:209-210`), hence `context` here and
            // `new_context` at the post-hook branch below.
            &tool_hook_context(context),
            base_env.clone(),
            Some(&context.abort_controller),
        )
        .await
    } else {
        ToolHookSeamResult::default()
    };
    let mut pre_tool_model_messages = pre_hook_result.model_messages.clone();
    let mut pre_tool_messages = pre_hook_result.messages;
    hook_messages.extend(pre_tool_messages.clone());

    let should_prevent_continuation =
        prepared_prevent_continuation || pre_hook_result.prevent_continuation;
    let continuation_stop_reason = pre_hook_result
        .stop_reason
        .as_deref()
        .or(prepared_stop_reason)
        .map(str::to_string);
    if pre_hook_result.stop_execution
        || matches!(
            pre_hook_result.permission_behavior,
            Some(crate::services::hooks::PermissionBehavior::Deny)
        )
    {
        response.choice = PermissionPromptChoice::Deny;
        response.permission_updates.clear();
    } else if matches!(
        pre_hook_result.permission_behavior,
        Some(crate::services::hooks::PermissionBehavior::Allow)
    ) {
        response.choice = PermissionPromptChoice::AllowOnce;
    }
    let mut effective_request = user_approved_request;
    // The configured entry also consumes runPreToolUseHooks directly. Keep
    // its resolving system decision intact (CC toolExecution.ts:831-837),
    // including the message used by the final denied tool_result (:1023).
    if let Some(message) = pre_hook_result.permission_message.as_ref() {
        effective_request.message = message.clone();
        response.decision_message = Some(message.clone());
    }
    if let Some(reason) = pre_hook_result.permission_decision_reason.as_ref() {
        effective_request.decision_reason = Some(reason.clone());
        effective_request.is_compound_command = false;
        response.decision_reason = Some(reason.clone());
    }
    let mut hook_input_permission_blocked = false;
    if let Some(updated_input) = pre_hook_result.updated_input.as_ref() {
        let adapted_input = if effective_request.tool_name == "Read" {
            HookInputRevalidation::Accepted(updated_input.clone())
        } else {
            revalidate_glob_grep_hook_input(&effective_request, updated_input, context)
        };
        match adapted_input {
            HookInputRevalidation::Passthrough => {}
            HookInputRevalidation::Accepted(updated_input) => {
                effective_request =
                    permission_request_with_updated_input(effective_request, updated_input);
                response.updated_input = Some(effective_request.input.clone());
            }
            HookInputRevalidation::Ask(updated_input)
            | HookInputRevalidation::Denied(updated_input) => {
                effective_request =
                    permission_request_with_updated_input(effective_request, updated_input);
                response.updated_input = Some(effective_request.input.clone());
                hook_input_permission_blocked = true;
            }
        }
    }
    if hook_input_permission_blocked {
        response.choice = PermissionPromptChoice::Deny;
        response.permission_updates.clear();
    }
    let effective_choice = response.choice;
    let (next_permission_context, permission_decision) = apply_prompt_response(
        &context.tool_permission_context,
        &effective_request,
        &response,
    );
    // CC `toolExecution.ts:979-993`, at its own position: after the permission
    // decision is final and before the `behavior !== 'allow'` branch, so the
    // row precedes both the deny result (`:1064`) and the allow one (`:1456`).
    if let Some(attachment) = hook_permission_decision_attachment(
        &effective_request,
        &response,
        permission_decision.behavior,
    ) {
        push_hook_permission_decision_attachment(
            attachment,
            &mut pre_tool_messages,
            &mut pre_tool_model_messages,
        );
    }
    let mut new_context = context.clone();
    new_context.update_permission_context(next_permission_context);
    // Maps to CC `toolExecution.ts:1207-1213`: the per-call context carries
    // `toolUseId` alongside `userModified` (same spread in CC).
    new_context.user_modified = Some(user_modified);
    new_context.tool_use_id = Some(effective_request.tool_use_id.clone());
    apply_tool_context_effects_after_execution(
        &mut new_context,
        &effective_request,
        effective_choice,
    );

    let execution_result = LocalToolExecutor
        .check_permissions_and_call_tool(
            &effective_request,
            &response,
            &new_context,
            assistant_message,
            None,
        )
        .await;
    let message = execution_result.message;
    let mut new_messages = execution_result.new_messages;
    let hook_tool_response = execution_result.hook_tool_response.clone();
    let model_content_blocks = execution_result.model_content_blocks;
    // Maps to CC `toolExecution.ts:1465-1470`: pair the modifier read at
    // :1400 with the executing tool_use id. CC attaches it to the pushed
    // result regardless of what PostToolUse hooks go on to do.
    let context_modifier = execution_result.context_modifier.map(|operation| {
        ToolContextModifier::from_operation(effective_request.tool_use_id.clone(), operation)
    });
    if let Some(cwd) = execution_result.cwd_after_update {
        crate::utils::shell::apply_cwd_after_command(&mut new_context, cwd);
    }
    if let Some(output) = execution_result.structured_output_update {
        push_structured_output_attachment(&mut new_messages, &output);
        new_context.structured_output = Some(output);
    }
    append_inline_skill_attachments(&effective_request, &mut new_context, &mut new_messages).await;
    let post_hook_result = if matches!(
        effective_choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    ) {
        match post_tool_hook_event(
            tool_result_status(message.as_ref()),
            new_context.abort_controller.is_aborted(),
        ) {
            PostToolHookEvent::Failure { is_interrupt } => {
                run_post_tool_use_failure_hooks(
                    &effective_request,
                    message.as_ref(),
                    is_interrupt,
                    hooks_config,
                    &tool_hook_context(&new_context),
                    base_env.clone(),
                    Some(&new_context.abort_controller),
                )
                .await
            }
            PostToolHookEvent::Success => {
                run_post_tool_use_hooks(
                    &effective_request,
                    message.as_ref(),
                    hook_tool_response.as_ref(),
                    hooks_config,
                    &tool_hook_context(&new_context),
                    base_env.clone(),
                    Some(&new_context.abort_controller),
                )
                .await
            }
        }
    } else if permission_denied_hook_is_applicable(&effective_request, effective_choice) {
        run_permission_denied_hooks_with_config(
            &effective_request,
            effective_choice,
            hooks_config,
            // CC `toolExecution.ts:1087` hands PermissionDenied the SAME
            // `permissionMode` const it captured at `:918`, before the
            // decision — unlike the two post-tool events above.
            &tool_hook_context(context),
            base_env.clone(),
            Some(&new_context.abort_controller),
        )
        .await
    } else {
        ToolHookSeamResult::default()
    };
    let post_tool_model_messages = post_hook_result.model_messages.clone();
    let post_tool_messages = post_hook_result.messages;
    hook_messages.extend(post_tool_messages.clone());

    let mut tool_result = append_accept_feedback_to_tool_result(
        message.as_ref().and_then(|message| {
            transcript_tool_result_to_model_message(message, &effective_request.tool_name)
        }),
        &response,
        effective_choice,
    );
    append_permission_content_blocks(&mut tool_result, &response);
    attach_model_content_blocks(&mut tool_result, model_content_blocks);
    // CC `toolExecution.ts` result yields (:857/:1069/:1465/:1733): every
    // tool_result records the uuid of its tool_use's assistant message.
    stamp_source_tool_assistant_uuid(&mut tool_result, assistant_message);
    let continuation_messages = if should_prevent_continuation
        && matches!(
            effective_choice,
            PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
        )
        && matches!(
            tool_result_status(message.as_ref()),
            Some(ToolResultStatus::Success)
        ) {
        vec![Message::Attachment(AttachmentMessage::new(
            serde_json::json!({
                "type": "hook_stopped_continuation",
                "message": continuation_stop_reason
                    .unwrap_or_else(|| "Execution stopped by hook".to_string()),
                "hookName": format!("PreToolUse:{}", effective_request.tool_name),
                "toolUseID": effective_request.tool_use_id,
                "hookEvent": "PreToolUse",
            }),
        ))]
    } else {
        Vec::new()
    };

    apply_tool_context_modifier(&mut new_context, context_modifier.as_ref());

    ToolExecutionResult {
        message,
        hook_messages,
        pre_tool_messages,
        pre_tool_model_messages,
        post_tool_messages,
        post_tool_model_messages,
        new_messages,
        continuation_messages,
        new_context,
        permission_decision,
        tool_result,
        context_modifier,
    }
}

/// Maps to: CC `toolExecution.ts:1272-1279` — a tool result carrying
/// `structured_output` also lands in the stream as a `structured_output`
/// attachment message (transcript-visible, null-rendered, normalized to []
/// for the API). The `ToolUseContext.structured_output` copy stays the
/// print/SDK result carrier until the event/context/attachment tracks
/// converge on the attachment (separate batch).
fn push_structured_output_attachment(new_messages: &mut Vec<Message>, output: &serde_json::Value) {
    new_messages.push(Message::Attachment(
        crate::types::message::AttachmentMessage::new(
            crate::types::message::Attachment::StructuredOutput {
                data: output.clone(),
            },
        ),
    ));
}

async fn append_inline_skill_attachments(
    request: &PermissionRequest,
    context: &mut ToolUseContext,
    new_messages: &mut Vec<Message>,
) {
    if request.tool_name != "Skill" {
        return;
    }
    let prompt = new_messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(
                user.content
                    .iter()
                    .filter_map(|content| match content {
                        UserContent::MetaText(text) | UserContent::Text(text) => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if prompt.is_empty() {
        return;
    }
    let attachments =
        crate::utils::attachments::process_user_input_attachments(&prompt, context).await;
    new_messages.extend(
        attachments
            .into_iter()
            .map(|attachment| attachment.model_message),
    );
}

/// Maps to: CC `services/tools/toolExecution.ts:1075-1078` — PermissionDenied
/// hooks run only for an auto-mode classifier denial:
///
/// ```ts
/// feature('TRANSCRIPT_CLASSIFIER') &&
/// permissionDecision.decisionReason?.type === 'classifier' &&
/// permissionDecision.decisionReason.classifier === 'auto-mode'
/// ```
///
/// This used to sniff a `"[ClassifierBlocked] "` prefix on `description`, a
/// string invented in `permissions.rs` because the classifier's `decisionReason`
/// was being dropped. With the producer restored, the real predicate is back.
fn permission_denied_hook_is_applicable(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
) -> bool {
    if choice != PermissionPromptChoice::Deny {
        return false;
    }
    if !crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
        return false;
    }
    matches!(
        &request.decision_reason,
        Some(crate::utils::permissions::permission_result::PermissionDecisionReason::Classifier {
            classifier,
            ..
        }) if classifier == "auto-mode"
    )
}

fn apply_tool_context_effects_after_execution(
    context: &mut ToolUseContext,
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
) {
    if !matches!(
        choice,
        PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow
    ) {
        return;
    }
    match request.tool_name.as_str() {
        "EnterPlanMode" => {
            context.tool_permission_context.mode = PermissionMode::Plan;
        }
        "ExitPlanMode" if context.tool_permission_context.mode == PermissionMode::Plan => {
            context.tool_permission_context.mode = PermissionMode::Default;
        }
        _ => {}
    }
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

fn tool_result_status(message: Option<&RenderableMessage>) -> Option<ToolResultStatus> {
    message
        .and_then(user_tool_result_block)
        .map(ToolResult::derived_status)
}

/// Which post-execution hook event CC's `checkPermissionsAndCallTool` reaches
/// once the tool call is over.
///
/// CC decides this by control flow, not by inspecting a status: the `try` at
/// `toolExecution.ts:1206` wraps `tool.call(...)`, so a call that RETURNS runs
/// PostToolUse (`:1483-1493`) and a call that THROWS lands in the `catch` at
/// `:1589`, which runs PostToolUseFailure (`:1700-1711`) for EVERY error it
/// caught. There is no third outcome inside that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostToolHookEvent {
    Success,
    Failure { is_interrupt: bool },
}

/// Maps to: CC `services/tools/toolExecution.ts:1206-1737` — the try/catch
/// split above, plus `:1694` `const isInterrupt = error instanceof AbortError`.
///
/// Rust returns where CC throws, so "the call threw" has to be read off the
/// result. The three port classifications reconcile like this:
///
/// - `Error` — the tool's own failure mapping. CC's `catch`. Fires.
/// - `Canceled` — the `CANCEL_MESSAGE` sentinel (`types/message.rs:594-604`).
///   Fires too, and for the same CC reason: a cancelled call did not return a
///   value. No tool mapper produces this today (the only two producers,
///   `run_tool_use_permission_gate_for_request_with_assistant` and
///   `check_permissions_and_call_tool_with_response_async`, are the port's
///   copies of CC's PRE-try abort gate at `:415-453`, which returns without
///   running any hook), so this arm is stated rather than exercised — it is
///   what keeps a future abort-mapping tool from silently skipping the event.
/// - `Rejected` / `Success` / no message — not failures at this position.
///   `Rejected` is reachable on the allow arm only through the Glob/Grep
///   compat reroute, whose CC counterpart is the `behavior !== 'allow'` deny
///   return at `:1064` — outside the try. Stays on the PostToolUse arm exactly
///   as before, because CC never reaches the catch for it.
///
/// `is_interrupt`: every `AbortError` that can reach CC's catch is thrown
/// immediately after a signal check — `bashPermissions.ts:1900`/`:1946`/
/// `:2400`, `TaskOutputTool.tsx:146`, `WebFetchTool/utils.ts:519`,
/// `runAgent.ts:809`, `AgentTool.tsx:1612` are all `if (signal.aborted) throw
/// new AbortError()`. (`permissions.ts:826`/`:1024`/`:1164` also throw one, but
/// from `hasPermissionsToUseTool`, which CC runs at `:921` — before the try —
/// so those never reach this classification.) The port's equivalent of "the
/// error was an AbortError" is therefore "the failing call ran under an aborted
/// controller".
///
/// Deviation: CC keys on the error's identity, so a genuine error thrown while
/// the signal happens to be set is still `isInterrupt: false`. This function
/// cannot tell those apart and reports `true`. Narrowing it needs an
/// abort-shaped failure carrier on `ToolCall`'s return, which is a per-tool
/// change; `bash_tool/mod.rs:949` already carries a private one
/// (`result.interrupted && reason() == Some("interrupt")`).
fn post_tool_hook_event(status: Option<ToolResultStatus>, aborted: bool) -> PostToolHookEvent {
    match status {
        Some(ToolResultStatus::Error) | Some(ToolResultStatus::Canceled) => {
            PostToolHookEvent::Failure {
                is_interrupt: aborted,
            }
        }
        _ => PostToolHookEvent::Success,
    }
}

/// The `createBaseHookInput(permissionMode, undefined, toolUseContext)` argument
/// triple every tool-event hook builder is called with — CC `hooks.ts:3419`,
/// `:3461`, `:3510`, `:3546`, `:4175`, resolved against `:301-328`.
///
/// This is the SINGLE constructor for the tool family's hook context, so every
/// tool-event builder's JSON payload (`create_base_hook_input_object`) derives
/// from the same value for one `ToolUseContext`.
///
/// Field by field, at the tool-hook call site specifically:
/// - `session_id`: CC passes `sessionId = undefined` (`:3419` et al), so
///   `createBaseHookInput` resolves `sessionId ?? getSessionId()` (`:315`) to
///   the MAIN session id. It is deliberately NOT `toolUseContext.agentId` —
///   that value is CC's *gate* key (`:3410`, `:3505`, `:3541`) and reaches the
///   payload as `agent_id`, never as `session_id`.
/// - `transcript_path`: `getTranscriptPathForSession(resolvedSessionId)`
///   (`:322`) — keyed on the session, so `get_transcript_path(None)`. A
///   subagent's own transcript reaches hooks through SubagentStop's
///   `agent_transcript_path` (`:3676`), not through the base.
/// - `cwd`: `getCwd()` (`:323`).
/// - `permission_mode`: the caller's `permissionMode`, which for every tool
///   event is `appState.toolPermissionContext.mode` — `toolHooks.ts:464-471`
///   (PreToolUse), `:52-53` (PostToolUse), `:209-210` (PostToolUseFailure),
///   `toolExecution.ts:918` + `:1087` (PermissionDenied), and
///   `structuredIO.ts:794-795` / `permissions.ts:400-406` /
///   `PermissionContext.ts:216-217` (PermissionRequest, where the last two
///   take it as a parameter).
/// - `agent_id` / `agent_type`: `toolUseContext.agentId` / `agentType`
///   (`:306`, `:319`, `:325-326`). `agent_type` has no
///   `getMainThreadAgentType()` fallback in this port — see
///   `create_base_hook_input_object`.
///
/// `session_id` and `transcript_path` used to be left at `Default::default()`
/// here; the JSON rail hid that (its `is_empty()` fallbacks re-derive both).
/// The env rail is CC's `{ ...subprocessEnv(), CLAUDE_PROJECT_DIR }`
/// (`hooks.ts:882-885`), which `exec_command_hook` sets itself; the
/// session/agent variables this port used to add there were removed on
/// 2026-10-03 (env C7).
pub(crate) fn tool_hook_context(context: &ToolUseContext) -> crate::services::hooks::HookContext {
    crate::services::hooks::HookContext {
        session_id: crate::bootstrap::state::get_session_id(),
        transcript_path: crate::utils::session_storage::get_transcript_path(None)
            .display()
            .to_string(),
        cwd: context.effective_cwd().display().to_string(),
        permission_mode: Some(
            crate::utils::permissions::permission_mode::to_external_permission_mode(
                context.tool_permission_context.mode,
            )
            .to_string(),
        ),
        agent_id: context.agent_id.clone(),
        agent_type: context.agent_type.clone(),
    }
}

/// Loads the tool family's hook table and its subprocess env.
///
/// The payload rail's other half — the `HookContext` the tool-event builders
/// spread — is NOT returned here. It is [`tool_hook_context`], a pure function
/// of the same `ToolUseContext` that every `*_with_config` hop already carries,
/// so both rails are the same value by construction and no call site can hand
/// the two halves a mismatched pair. The tool family adds nothing to the hook
/// env (`exec_command_hook` sets `CLAUDE_PROJECT_DIR`), so `base_env` is empty.
fn load_tool_hooks_config_and_env(
    context: &ToolUseContext,
) -> Option<(RegisteredHooks, Vec<(String, String)>)> {
    let loaded_hooks = crate::services::hooks::load_hooks_config();
    let mut config = loaded_hooks.config;
    if !loaded_hooks.allow_managed_hooks_only {
        // CC reads the session id as `toolUseContext.agentId ?? getSessionId()`
        // — six times in hooks.ts (`:2003 :3409 :3504 :3540 :3655 :3836`), plus
        // four that call `getSessionId()` outright. Not one of them skips the
        // lookup when there is no agentId. Gating the merge on `Some(agent_id)`
        // therefore hid every session hook the MAIN session registers, which is
        // what `register_skill_hooks` and the channel path do.
        //
        // This is the GATE key, and it is a different value from the payload's
        // `session_id` — see [`tool_hook_context`].
        let session_id = context
            .agent_id
            .clone()
            .unwrap_or_else(crate::bootstrap::state::get_session_id);
        crate::utils::hooks::session_hooks::merge_session_hooks_into_config(
            &mut config,
            &session_id,
        );
    }
    if config.is_empty() {
        return None;
    }
    Some((config, Vec::new()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingToolResultBlock {
    pub message: RenderableMessage,
    pub tool_result: UserMessage,
}

/// Maps to CC `query.ts` `yieldMissingToolResultBlocks(...)` while keeping
/// model `tool_result` construction inside the tool execution boundary.
/// Maps to: CC stamping `sourceToolAssistantUUID: assistantMessage.uuid` on
/// every tool_result user message it creates — `query.ts:145` for
/// interruption results and the `toolExecution.ts` yields (:407, :449, :486,
/// :676, :729, :857, :1069, :1465, :1733). Rust's assistant envelope uuid is
/// the `AssistantMessage.uuid` field; empty = pre-C3a data, left
/// unstamped. Cometix-only branches with no CC counterpart (the
/// COMETIX_WRITE_ENABLED mutation gates) keep `None`.
fn stamp_source_tool_assistant_uuid(
    tool_result: &mut Option<UserMessage>,
    assistant_message: Option<&AssistantMessage>,
) {
    let Some(assistant) = assistant_message else {
        return;
    };
    if assistant.uuid.is_empty() {
        return;
    }
    if let Some(result) = tool_result.as_mut() {
        result.source_tool_assistant_uuid = Some(assistant.uuid.clone());
    }
}

pub fn yield_missing_tool_result_blocks(
    assistant_messages: &[AssistantMessage],
    error_message: &str,
) -> Vec<MissingToolResultBlock> {
    let mut results = Vec::new();
    for assistant_message in assistant_messages {
        for content in &assistant_message.content {
            let crate::types::message::AssistantContent::ToolUse(tool_use) = content else {
                continue;
            };
            let message = RenderableMessage::user_tool_result(
                uuid::Uuid::new_v4().to_string(),
                tool_use.id.0.clone(),
                error_message,
                true,
            );
            let mut tool_result = transcript_tool_result_to_model_message(&message, &tool_use.name);
            // CC `query.ts:145`: the interruption tool_result records the
            // uuid of the assistant message that carried the tool_use.
            stamp_source_tool_assistant_uuid(&mut tool_result, Some(assistant_message));
            let Some(tool_result) = tool_result else {
                continue;
            };
            results.push(MissingToolResultBlock {
                message,
                tool_result,
            });
        }
    }
    results
}

fn web_fetch_model_content(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| content.to_string())
}

fn web_search_model_content(content: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };
    let query = value
        .get("query")
        .and_then(|query| query.as_str())
        .unwrap_or_default();
    let mut formatted = format!("Web search results for query: \"{query}\"\n\n");
    if let Some(results) = value.get("results").and_then(|results| results.as_array()) {
        for result in results.iter().filter(|result| !result.is_null()) {
            if let Some(text) = result.as_str() {
                formatted.push_str(text);
                formatted.push_str("\n\n");
            } else if let Some(content) = result.get("content") {
                if content.as_array().is_some_and(|items| !items.is_empty()) {
                    formatted.push_str("Links: ");
                    formatted.push_str(&content.to_string());
                    formatted.push_str("\n\n");
                } else {
                    formatted.push_str("No links found.\n\n");
                }
            }
        }
    }
    formatted.push_str(
        "\nREMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.",
    );
    formatted.trim().to_string()
}

fn worktree_model_content(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| content.to_string())
}

fn ask_user_question_model_content(content: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
        // New typed AskUserQuestion execution has already passed through
        // `AskUserQuestionTool.mapToolResultToToolResultBlockParam(...)`.
        return content.to_string();
    };
    let Some(answer_map) = parsed.get("answers").and_then(|value| value.as_object()) else {
        return content.to_string();
    };
    let answers = answer_map
        .iter()
        .map(|(question_text, answer)| {
            let answer = answer
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| answer.to_string());
            let annotation = parsed
                .get("annotations")
                .and_then(|value| value.get(question_text));
            let mut parts = vec![format!("\"{question_text}\"=\"{answer}\"")];
            if let Some(preview) = annotation
                .and_then(|value| value.get("preview"))
                .and_then(|value| value.as_str())
            {
                parts.push(format!("selected preview:\n{preview}"));
            }
            if let Some(notes) = annotation
                .and_then(|value| value.get("notes"))
                .and_then(|value| value.as_str())
            {
                parts.push(format!("user notes: {notes}"));
            }
            parts.join(" ")
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "User has answered your questions: {answers}. You can now continue with the user's answers in mind."
    )
}

/// The row no longer stores a lookups-derived tool name, so the caller
/// supplies it — mirroring CC, where per-tool model mapping is
/// `Tool.mapToolResultToToolResultBlockParam(...)` invoked with the tool in
/// hand (`services/tools/toolExecution.ts`), never derived from the message.
pub(crate) fn transcript_tool_result_to_model_message(
    message: &RenderableMessage,
    tool_name: &str,
) -> Option<UserMessage> {
    let block = user_tool_result_block(message)?;
    if block.tool_use_id.0.is_empty() {
        return None;
    }
    let tool_use_id = &block.tool_use_id.0;
    let status = block.derived_status();
    let content = &block.content;

    let mut content_blocks = Vec::new();
    let model_content = if tool_name
        == crate::tools::ask_user_question_tool::prompt::ASK_USER_QUESTION_TOOL_NAME
        && matches!(status, ToolResultStatus::Success)
    {
        // Maps to CC `AskUserQuestionTool.mapToolResultToToolResultBlockParam(...)`.
        ask_user_question_model_content(content)
    } else if tool_name == "WebFetch" {
        // Maps to CC `WebFetchTool.mapToolResultToToolResultBlockParam(...)`.
        web_fetch_model_content(content)
    } else if tool_name == "WebSearch" {
        // Maps to CC `WebSearchTool.mapToolResultToToolResultBlockParam(...)`.
        web_search_model_content(content)
    } else if matches!(tool_name, "EnterWorktree" | "ExitWorktree") {
        worktree_model_content(content)
    } else if matches!(tool_name, "SendUserMessage" | "Brief")
        && matches!(status, ToolResultStatus::Success)
    {
        // Maps to CC `BriefTool.mapToolResultToToolResultBlockParam(...)`.
        // New typed execution already records the official acknowledgement;
        // legacy recovered transcripts may still contain the user-visible
        // message and need the old compact-model remap.
        if content.starts_with("Message delivered to user.") {
            content.clone()
        } else {
            "Message delivered to user.".to_string()
        }
    } else if tool_name == crate::tools::tool_search_tool::prompt::TOOL_SEARCH_TOOL_NAME
        && matches!(status, ToolResultStatus::Success)
    {
        // Maps to CC `ToolSearchTool.mapToolResultToToolResultBlockParam(...)`:
        // successful matches become `tool_reference` blocks so the API can
        // expand the matched deferred tool schemas in the next request.
        let tool_search_output = serde_json::from_str::<serde_json::Value>(content).ok();
        let matches = tool_search_output
            .as_ref()
            .and_then(|value| value.get("matches").cloned())
            .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .unwrap_or_default();
        if matches.is_empty() {
            let pending_mcp_servers = tool_search_output
                .as_ref()
                .and_then(|value| value.get("pending_mcp_servers").cloned())
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .unwrap_or_default();
            if pending_mcp_servers.is_empty() {
                "No matching deferred tools found".to_string()
            } else {
                format!(
                    "No matching deferred tools found. Some MCP servers are still connecting: {}. Their tools will become available shortly — try searching again.",
                    pending_mcp_servers.join(", ")
                )
            }
        } else {
            content_blocks = matches
                .into_iter()
                .map(
                    |tool_name| crate::types::message::ToolResultContentBlock::ToolReference {
                        tool_name,
                    },
                )
                .collect();
            content.clone()
        }
    } else {
        content.clone()
    };
    let model_content = if crate::utils::tool_result_storage::is_tool_result_content_empty(
        &model_content,
        &content_blocks,
    ) {
        crate::utils::tool_result_storage::empty_tool_result_content_marker(tool_name)
    } else {
        model_content
    };
    let model_content = if matches!(status, ToolResultStatus::Success) && content_blocks.is_empty()
    {
        let declared_max = find_tool_call(tool_name)
            .map(|tool| tool.max_result_size_chars())
            .unwrap_or(crate::tool::DEFAULT_MAX_RESULT_SIZE_CHARS);
        crate::utils::tool_result_storage::maybe_persist_large_tool_result_text(
            &model_content,
            tool_name,
            tool_use_id,
            declared_max,
        )
    } else {
        model_content
    };

    Some(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::ToolResult(ToolResult {
            tool_use_id: crate::types::ids::ToolUseId(tool_use_id.clone()),
            content: model_content,
            is_error: !matches!(status, ToolResultStatus::Success),
            content_blocks,
            // Every tool's producer stores the raw value on the
            // row itself (CC's direction) — there is no display derivation.
            tool_use_result: block.tool_use_result.clone(),
        })],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;
    use crate::types::message::{
        RenderableMessage, RenderableMessageKind, SystemMessage, ToolUseStatus,
    };
    use crate::types::permissions::PermissionRuleValue;
    use crate::utils::permissions::permissions::mock_permission_request;
    use crate::utils::test_env::{EnvVarGuard, IsolatedProjectSettings, TEST_ENV_LOCK};
    use std::collections::HashMap;

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

    /// Maps to: CC `services/tools/toolExecution.ts:1075-1078` — PermissionDenied
    /// hooks run for `feature('TRANSCRIPT_CLASSIFIER') && decisionReason.type ===
    /// 'classifier' && decisionReason.classifier === 'auto-mode'`, and for no
    /// other denial. The predicate used to sniff a `"[ClassifierBlocked] "`
    /// prefix on `description`, a Rust-only string invented because the
    /// classifier's `decisionReason` was dropped upstream.
    #[test]
    fn permission_denied_hooks_match_official_auto_mode_classifier_reason() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");

        let with_reason = |reason: Option<
            crate::utils::permissions::permission_result::PermissionDecisionReason,
        >| {
            let mut request = mock_permission_request(
                "perm",
                "toolu",
                "Bash",
                "npm publish",
                crate::types::permissions::PermissionMode::Auto,
            );
            request.decision_reason = reason;
            request
        };
        let classifier = |name: &str| {
            Some(
                crate::utils::permissions::permission_result::PermissionDecisionReason::Classifier {
                    classifier: name.to_string(),
                    reason: "blocked".to_string(),
                },
            )
        };

        let auto_mode_denied = with_reason(classifier("auto-mode"));
        let other_classifier = with_reason(classifier("dangerous-agent-action"));
        let no_reason = with_reason(None);
        // The old carrier: a description prefix must no longer decide anything.
        let mut prefixed = with_reason(None);
        prefixed.description = "[ClassifierBlocked] blocked".to_string();

        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");

        assert!(permission_denied_hook_is_applicable(
            &auto_mode_denied,
            PermissionPromptChoice::Deny
        ));
        assert!(
            !permission_denied_hook_is_applicable(
                &auto_mode_denied,
                PermissionPromptChoice::AllowOnce
            ),
            "CC :995 only reaches this block when the decision was not an allow"
        );
        assert!(
            !permission_denied_hook_is_applicable(&other_classifier, PermissionPromptChoice::Deny),
            "CC :1078 pins classifier === 'auto-mode'"
        );
        assert!(!permission_denied_hook_is_applicable(
            &no_reason,
            PermissionPromptChoice::Deny
        ));
        assert!(
            !permission_denied_hook_is_applicable(&prefixed, PermissionPromptChoice::Deny),
            "the description prefix is no longer the carrier"
        );
    }

    /// The hook loading chain was `#[cfg(not(test))]` until 2026-08-19 — the
    /// whole path (five-source settings merge, session-hook merge, the
    /// managed-only gate, HookContext/env construction) ran in no test build.
    /// This is its first direct execution: the pinned harness environment
    /// (CLAUDE_CONFIG_DIR + COMETIX_TEST_PROJECT_DIR) has no hooks, so the
    /// loader returns None; registering a session hook makes the merge leg
    /// produce a config, exactly as CC merges `getSessionHooks` into the
    /// loaded settings (hooks.ts session merge the doc comment cites).
    #[test]
    fn tool_hook_loader_reads_the_pinned_environment_and_session_hooks() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _settings = IsolatedProjectSettings::pin();
        let context = crate::tool::ToolUseContext::default();

        // The pinned environment configures no hooks anywhere.
        assert!(load_tool_hooks_config_and_env(&context).is_none());

        // A session hook rides the merge leg into the loaded config.
        let session_id = crate::bootstrap::state::get_session_id();
        crate::utils::hooks::session_hooks::add_session_hook(
            &session_id,
            crate::services::hooks::HookEvent::PreToolUse,
            "Bash",
            crate::services::hooks::HookCommand {
                command: "true".to_string(),
                shell: None,
                timeout: None,
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );
        let loaded = load_tool_hooks_config_and_env(&context);
        crate::utils::hooks::session_hooks::clear_session_hooks(&session_id);
        let (config, base_env) = loaded.expect("session hook must surface through the loader");
        assert!(!config.is_empty());
        // `exec_command_hook` sets `CLAUDE_PROJECT_DIR` for every caller
        // (CC `hooks.ts:816,882-885`); the tool family adds nothing.
        assert!(base_env.is_empty(), "{base_env:?}");
    }

    /// Maps to: CC `utils/hooks.ts:301-328#createBaseHookInput`, resolved for
    /// the arguments every tool-event builder passes it —
    /// `createBaseHookInput(permissionMode, undefined, toolUseContext)`
    /// (`:3419`, `:3461`, `:3510`, `:3546`, `:4175`).
    ///
    /// BOTH rails are asserted from ONE hook run, because they are two
    /// projections of the same `HookContext`. The JSON rail carries the session,
    /// transcript, cwd and agent fields; the env rail is CC's
    /// `{ ...subprocessEnv(), CLAUDE_PROJECT_DIR }` (`hooks.ts:882-885`) and
    /// carries none of them.
    ///
    /// `agent_id` / `agent_type` / `permission_mode` (CC `:324-326`) are the
    /// A2 half: PreToolUse could carry none of them before, so a hook could not
    /// tell a subagent's tool call from the main thread's.
    #[cfg(unix)]
    #[tokio::test]
    async fn tool_hook_input_carries_the_official_base_fields_and_env_only_the_project_dir() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        // A host value would be inherited through `subprocessEnv()`, as in CC.
        let _absent = [
            "CLAUDE_SESSION_ID",
            "CLAUDE_TRANSCRIPT_PATH",
            "CLAUDE_AGENT_ID",
        ]
        .map(EnvVarGuard::unset);
        let _settings = IsolatedProjectSettings::pin();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();

        let input_capture = std::env::temp_dir().join(format!(
            "cometix-tool-hook-input-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let env_capture = std::env::temp_dir().join(format!(
            "cometix-tool-hook-env-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));

        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..Default::default()
        });
        context.agent_id = Some("agent_rails".to_string());
        context.agent_type = Some("code-reviewer".to_string());

        // The command embeds its capture paths: `base_env` on this path is the
        // production one `load_tool_hooks_config_and_env` builds, which is
        // exactly what the env half is here to inspect.
        let session_id = crate::bootstrap::state::get_session_id();
        crate::utils::hooks::session_hooks::add_session_hook(
            // The loader's GATE key is `agentId ?? getSessionId()`
            // (`hooks.ts:2003`), so the registration has to use the agent id.
            "agent_rails",
            crate::services::hooks::HookEvent::PreToolUse,
            "Bash",
            crate::services::hooks::HookCommand {
                command: format!(
                    "cat > '{}'; printf '%s\\n%s\\n%s\\n%s\\n' \"${{CLAUDE_SESSION_ID-<unset>}}\" \"${{CLAUDE_TRANSCRIPT_PATH-<unset>}}\" \"${{CLAUDE_AGENT_ID-<unset>}}\" \"$CLAUDE_PROJECT_DIR\" > '{}'",
                    input_capture.display(),
                    env_capture.display()
                ),
                shell: None,
                timeout: Some(10),
                condition: None,
                status: None,
                once: None,
                is_async: None,
                async_rewake: None,
            },
        );

        let mut queue = Vec::new();
        let request = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue)
            .request
            .or_else(|| queue.pop().map(|confirm| confirm.request))
            .expect("the gate produces a Bash permission request");
        prepare_permission_request_before_prompt(&request, &context).await;
        crate::utils::hooks::session_hooks::clear_session_hooks("agent_rails");

        let payload: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&input_capture).expect("the PreToolUse hook received its stdin"),
        )
        .expect("the hook input is JSON");
        let env = std::fs::read_to_string(&env_capture).expect("the hook recorded its env");
        let _ = std::fs::remove_file(&input_capture);
        let _ = std::fs::remove_file(&env_capture);
        let env_lines = env.lines().collect::<Vec<_>>();

        // CC passes `sessionId = undefined`, so `sessionId ?? getSessionId()`
        // (`:315`) is the MAIN session — never `toolUseContext.agentId`, which
        // is the gate key above and reaches the payload as `agent_id`.
        assert!(!session_id.is_empty(), "precondition: a live session id");
        assert_eq!(payload["session_id"], serde_json::json!(session_id));

        // `getTranscriptPathForSession(resolvedSessionId)` (`:322`).
        let transcript_path = crate::utils::session_storage::get_transcript_path(None)
            .display()
            .to_string();
        assert!(!transcript_path.is_empty());
        assert_eq!(
            payload["transcript_path"],
            serde_json::json!(transcript_path)
        );

        // `getCwd()` (`:323`).
        let cwd = context.effective_cwd().display().to_string();
        assert_eq!(payload["cwd"], serde_json::json!(cwd));

        // The A2 triple CC reads off `toolUseContext` + `permissionMode`.
        assert_eq!(payload["agent_id"], "agent_rails");
        assert_eq!(payload["agent_type"], "code-reviewer");
        assert_eq!(payload["permission_mode"], "acceptEdits");

        // The env rail: `CLAUDE_PROJECT_DIR` only, from the project root that
        // `exec_command_hook` reads (the original cwd stands in for it).
        let project_dir = crate::bootstrap::state::get_original_cwd()
            .display()
            .to_string();
        assert_eq!(
            env_lines,
            ["<unset>", "<unset>", "<unset>", project_dir.as_str()],
            "hook env rail"
        );
    }

    /// Maps to: CC `services/tools/toolExecution.ts:1206` (the `try` that wraps
    /// `tool.call`), `:1589` (the `catch`), and `:1694` (`const isInterrupt =
    /// error instanceof AbortError`).
    ///
    /// Old shape: the branch was `tool_result_status(...) == Some(Error)` with
    /// no interrupt notion at all, so `Canceled` silently took the PostToolUse
    /// arm and `is_interrupt` had no producer. The `aborted` column below is
    /// what fails first against it.
    #[test]
    fn post_tool_hook_event_matches_the_official_try_catch_split() {
        use PostToolHookEvent::{Failure, Success};

        // The call returned: CC runs PostToolUse (`:1483-1493`).
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Success), false),
            Success
        );
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Success), true),
            Success,
            "an aborted turn does not turn a returned value into a throw"
        );
        // No tool_result row at all is not a caught error either.
        assert_eq!(post_tool_hook_event(None, true), Success);
        // CC's deny result returns at `:1064`, before the try — the Glob/Grep
        // compat reroute is this port's only allow-arm producer of it.
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Rejected), true),
            Success
        );

        // The call threw: CC runs PostToolUseFailure for EVERY caught error and
        // tags only the abort one.
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Error), false),
            Failure {
                is_interrupt: false
            }
        );
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Error), true),
            Failure { is_interrupt: true }
        );
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Canceled), true),
            Failure { is_interrupt: true },
            "a cancelled call did not return a value either"
        );
        assert_eq!(
            post_tool_hook_event(Some(ToolResultStatus::Canceled), false),
            Failure {
                is_interrupt: false
            }
        );
    }

    fn queued_bash() -> RenderableMessage {
        RenderableMessage::assistant_block(
            "toolu_mock",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(String::new()),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "printf permission-gated"}),
            }),
        )
    }

    // Every tool's raw projection lives on its trait
    // (`ToolCall::tool_use_result`) and subagent persistence gates the row
    // value directly (`preserve_raw.then(...)` at the live seam) —
    // `tool_use_result_from_display` and the RawOmitted wrapper are gone.

    #[test]
    fn grep_zod_v4_issue_projection_matches_strict_semantic_schema() {
        let error = carrier_parsed_input(
            "Grep",
            crate::tools::grep_tool::input_schema(),
            &serde_json::json!({
                "pattern": 1,
                "path": false,
                "output_mode": "bad",
                "-B": "x",
                "-i": 0,
                "unexpected": true
            }),
        )
        .expect_err("invalid Grep input");
        assert_eq!(
            error.raw,
            r#"[
  {
    "expected": "string",
    "code": "invalid_type",
    "path": [
      "pattern"
    ],
    "message": "Invalid input: expected string, received number"
  },
  {
    "expected": "string",
    "code": "invalid_type",
    "path": [
      "path"
    ],
    "message": "Invalid input: expected string, received boolean"
  },
  {
    "code": "invalid_value",
    "values": [
      "content",
      "files_with_matches",
      "count"
    ],
    "path": [
      "output_mode"
    ],
    "message": "Invalid option: expected one of \"content\"|\"files_with_matches\"|\"count\""
  },
  {
    "expected": "number",
    "code": "invalid_type",
    "path": [
      "-B"
    ],
    "message": "Invalid input: expected number, received string"
  },
  {
    "expected": "boolean",
    "code": "invalid_type",
    "path": [
      "-i"
    ],
    "message": "Invalid input: expected boolean, received number"
  },
  {
    "code": "unrecognized_keys",
    "keys": [
      "unexpected"
    ],
    "path": [],
    "message": "Unrecognized key: \"unexpected\""
  }
]"#
        );
        assert_eq!(
            error.formatted,
            "Grep failed due to the following issues:\nAn unexpected parameter `unexpected` was provided\nThe parameter `pattern` type is expected as `string` but provided as `number`\nThe parameter `path` type is expected as `string` but provided as `boolean`\nThe parameter `-B` type is expected as `number` but provided as `string`\nThe parameter `-i` type is expected as `boolean` but provided as `number`"
        );

        let enum_only = carrier_parsed_input(
            "Grep",
            crate::tools::grep_tool::input_schema(),
            &serde_json::json!({
                "pattern": "x",
                "output_mode": "bad"
            }),
        )
        .expect_err("bad enum");
        assert_eq!(enum_only.formatted, enum_only.raw);
    }

    /// CC `AgentTool.tsx:165` is a plain `z.object` — its `safeParse` ACCEPTS
    /// and STRIPS unknown fields (`toolExecution.ts:615` → `:761`
    /// `processedInput = parsedInput.data`). The JSON-Schema fallback rejects
    /// them (`additionalProperties: false` on both object kinds), which is the
    /// divergence #144 closes: Agent must be a native carrier whose parse
    /// RESULT replaces the downstream input.
    #[test]
    fn agent_carrier_strips_unknown_fields_instead_of_rejecting() {
        let parsed = validate_tool_input_for_execution(
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
            &serde_json::json!({
                "description": "run tests",
                "prompt": "run the suite",
                "made_up_field": true,
            }),
            // The JSON-Schema argument must not decide the outcome for a
            // carrier tool — hand the projection that WOULD reject.
            &crate::utils::zod_to_json_schema::zod_to_json_schema(
                crate::tools::agent_tool::input_schema(),
            ),
        )
        .expect("a plain z.object accepts unknown fields")
        .expect("the carrier returns parsedInput.data");
        assert_eq!(parsed.get("made_up_field"), None, "unknown field stripped");
        assert_eq!(
            parsed.get("description"),
            Some(&serde_json::json!("run tests"))
        );
        assert_eq!(
            parsed.get("prompt"),
            Some(&serde_json::json!("run the suite"))
        );

        // Stripping is not leniency: required fields are still enforced.
        assert!(
            validate_tool_input_for_execution(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                &serde_json::json!({"description": "no prompt"}),
                &serde_json::Value::Null,
            )
            .is_err(),
            "a missing required `prompt` still fails the parse"
        );
    }

    #[test]
    fn read_schema_error_preserves_zod_v4_raw_issues_and_formatted_summary() {
        let error = carrier_parsed_input(
            "Read",
            crate::tools::file_read_tool::input_schema(),
            &serde_json::json!({
                "file_path": 5,
                "offset": 1.5,
                "limit": 0,
                "pages": false,
                "extra": true
            }),
        )
        .expect_err("invalid Read input");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&error.raw).unwrap(),
            serde_json::json!([
                {
                    "expected": "string",
                    "code": "invalid_type",
                    "path": ["file_path"],
                    "message": "Invalid input: expected string, received number"
                },
                {
                    "expected": "int",
                    "format": "safeint",
                    "code": "invalid_type",
                    "path": ["offset"],
                    "message": "Invalid input: expected int, received number"
                },
                {
                    "origin": "number",
                    "code": "too_small",
                    "minimum": 0,
                    "inclusive": false,
                    "path": ["limit"],
                    "message": "Too small: expected number to be >0"
                },
                {
                    "expected": "string",
                    "code": "invalid_type",
                    "path": ["pages"],
                    "message": "Invalid input: expected string, received boolean"
                },
                {
                    "code": "unrecognized_keys",
                    "keys": ["extra"],
                    "path": [],
                    "message": "Unrecognized key: \"extra\""
                }
            ])
        );
        assert_eq!(
            error.formatted,
            "Read failed due to the following issues:\nAn unexpected parameter `extra` was provided\nThe parameter `file_path` type is expected as `string` but provided as `number`\nThe parameter `offset` type is expected as `int` but provided as `number`\nThe parameter `pages` type is expected as `string` but provided as `boolean`"
        );
        let missing = carrier_parsed_input(
            "Read",
            crate::tools::file_read_tool::input_schema(),
            &serde_json::json!({}),
        )
        .expect_err("missing path");
        assert_eq!(
            missing.formatted,
            "Read failed due to the following issue:\nThe required parameter `file_path` is missing"
        );
    }

    #[test]
    fn read_initial_schema_corpus_matches_official_strict_semantic_numbers() {
        let valid = [
            serde_json::json!({"file_path": "a"}),
            serde_json::json!({"file_path": "a", "offset": 0, "limit": 1}),
            serde_json::json!({"file_path": "a", "offset": "2", "limit": "3.0"}),
            serde_json::json!({
                "file_path": "a",
                "offset": 9_007_199_254_740_991_u64,
                "limit": "9007199254740991",
                "pages": ""
            }),
        ];
        for input in valid {
            let normalized = crate::tool::ToolCall::normalize_input(
                &crate::tools::file_read_tool::FileReadTool,
                &input,
            );
            assert!(
                carrier_parsed_input(
                    "Read",
                    crate::tools::file_read_tool::input_schema(),
                    &normalized,
                )
                .is_ok(),
                "unexpected rejection for {input}: {normalized}"
            );
        }

        let invalid = [
            serde_json::json!({"file_path": "a", "offset": "+1"}),
            serde_json::json!({"file_path": "a", "offset": "1e2"}),
            serde_json::json!({"file_path": "a", "offset": " 1"}),
            serde_json::json!({"file_path": "a", "offset": ""}),
            serde_json::json!({"file_path": "a", "offset": null}),
            serde_json::json!({"file_path": "a", "offset": true}),
            serde_json::json!({"file_path": "a", "offset": [1]}),
            serde_json::json!({"file_path": "a", "offset": 1.5}),
            serde_json::json!({"file_path": "a", "offset": -1}),
            serde_json::json!({"file_path": "a", "limit": 0}),
            serde_json::json!({"file_path": "a", "limit": false}),
            serde_json::json!({"file_path": "a", "pages": []}),
            serde_json::json!({"file_path": "a", "offset": 9_007_199_254_740_992_u64}),
            serde_json::json!({"file_path": "a", "offset": "9007199254740993"}),
            serde_json::json!({"file_path": "a", "extra": true}),
        ];
        for input in invalid {
            let normalized = crate::tool::ToolCall::normalize_input(
                &crate::tools::file_read_tool::FileReadTool,
                &input,
            );
            assert!(
                carrier_parsed_input(
                    "Read",
                    crate::tools::file_read_tool::input_schema(),
                    &normalized,
                )
                .is_err(),
                "unexpected acceptance for {input}: {normalized}"
            );
        }
    }

    // The Grep raw projection now lives on the trait
    // (`GrepTool::tool_use_result`, asserted field-complete in
    // `grep_tool::tests::grep_model_mapping_and_display_preserve_all_output_fields`),
    // so `tool_use_result_from_display` no longer has a Grep arm to test.

    #[test]
    fn glob_large_model_result_is_persisted_at_official_effective_threshold() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let config = std::env::temp_dir().join(format!(
            "cometix-glob-persist-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&config).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", config.to_string_lossy().as_ref());
        let _overrides = EnvVarGuard::unset("CLAUDE_INTERNAL_FC_OVERRIDES");
        let content = "p".repeat(60_000);
        // The raw `toolUseResult` rides the row (trait projection), and
        // the display records only the emit-gate decision.
        let raw = serde_json::json!({
            "durationMs": 1,
            "numFiles": 1,
            "filenames": [content.clone()],
            "truncated": false
        });
        let message = RenderableMessage::user_tool_result(
            "glob-large",
            "toolu_glob_large",
            content.clone(),
            false,
        )
        .with_tool_use_result(Some(raw.clone()));
        let model =
            transcript_tool_result_to_model_message(&message, "Glob").expect("model result");
        let Some(UserContent::ToolResult(result)) = model.content.first() else {
            panic!("expected tool result");
        };
        assert!(
            result
                .content
                .starts_with(crate::utils::tool_result_storage::PERSISTED_OUTPUT_TAG)
        );
        let persisted =
            crate::utils::tool_result_storage::get_tool_result_path("toolu_glob_large", false);
        assert_eq!(std::fs::read_to_string(&persisted).unwrap(), content);
        assert_eq!(result.tool_use_result, Some(raw));
        let _ = std::fs::remove_dir_all(config);
    }

    #[test]
    fn grep_large_model_result_uses_its_20k_persistence_threshold() {
        let _lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config = std::env::temp_dir().join(format!(
            "cometix-grep-persist-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&config).unwrap();
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", config.to_string_lossy().as_ref());
        let _overrides = EnvVarGuard::unset("CLAUDE_INTERNAL_FC_OVERRIDES");
        let content = "g".repeat(25_000);
        // The raw `toolUseResult` rides the row (trait projection), and
        // the display records only the emit-gate decision.
        let raw = serde_json::json!({
            "mode": "content",
            "numFiles": 0,
            "filenames": [],
            "content": content.clone(),
            "numLines": 1
        });
        let message = RenderableMessage::user_tool_result(
            "grep-large",
            "toolu_grep_large",
            content.clone(),
            false,
        )
        .with_tool_use_result(Some(raw.clone()));
        let model =
            transcript_tool_result_to_model_message(&message, "Grep").expect("model result");
        let Some(UserContent::ToolResult(result)) = model.content.first() else {
            panic!("expected tool result");
        };
        assert!(
            result
                .content
                .starts_with(crate::utils::tool_result_storage::PERSISTED_OUTPUT_TAG)
        );
        let persisted =
            crate::utils::tool_result_storage::get_tool_result_path("toolu_grep_large", false);
        assert_eq!(std::fs::read_to_string(&persisted).unwrap(), content);
        assert_eq!(result.tool_use_result, Some(raw));
        let _ = std::fs::remove_dir_all(config);
    }

    #[test]
    fn glob_semantic_validation_preserves_error_tool_use_result_before_permission() {
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-validation-executor-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        let tool_use = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-glob-validation".to_string()),
            name: "Glob".to_string(),
            input: serde_json::json!({"pattern": "*", "path": "missing"}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                tool_use.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut permission_queue = Vec::new();
        let update = run_tool_use(&tool_use, &assistant, &context, &mut permission_queue);
        assert!(!update.blocked_on_permission);
        assert!(permission_queue.is_empty());
        let block = update
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("tool result block");
        assert_eq!(block.derived_status(), ToolResultStatus::Error);
        // The raw `Error: …` string rides the row, not a display variant.
        assert!(matches!(
            block.tool_use_result.as_ref(),
            Some(serde_json::Value::String(raw))
                if raw.starts_with("Error: Directory does not exist: missing.")
        ));
        assert!(
            block
                .content
                .contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE)
        );
        assert!(matches!(
            update.tool_result.as_ref().and_then(|message| message.content.first()),
            Some(UserContent::ToolResult(result))
                if matches!(
                    result.tool_use_result.as_ref(),
                    Some(serde_json::Value::String(raw))
                        if raw.starts_with("Error: Directory does not exist: missing.")
                )
        ));

        let invalid_schema_use = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-glob-schema".to_string()),
            name: "Glob".to_string(),
            input: serde_json::json!({"path": ".", "unexpected": true}),
        };
        let invalid_schema = run_tool_use(
            &invalid_schema_use,
            &assistant,
            &context,
            &mut permission_queue,
        );
        let expected_raw = r#"[
  {
    "expected": "string",
    "code": "invalid_type",
    "path": [
      "pattern"
    ],
    "message": "Invalid input: expected string, received undefined"
  },
  {
    "code": "unrecognized_keys",
    "keys": [
      "unexpected"
    ],
    "path": [],
    "message": "Unrecognized key: \"unexpected\""
  }
]"#;
        let block = invalid_schema
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("tool result block");
        assert!(matches!(
            block.tool_use_result.as_ref(),
            Some(serde_json::Value::String(raw))
                if raw == &format!("InputValidationError: {expected_raw}")
        ));
        assert_eq!(
            block.content,
            "<tool_use_error>InputValidationError: Glob failed due to the following issues:\nThe required parameter `pattern` is missing\nAn unexpected parameter `unexpected` was provided</tool_use_error>"
        );

        let fatal_use = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-glob-fatal".to_string()),
            name: "Glob".to_string(),
            input: serde_json::json!({"pattern": "*", "path": "bad\u{0}path"}),
        };
        let fatal = run_tool_use(&fatal_use, &assistant, &context, &mut permission_queue);
        let block = fatal
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("tool result block");
        assert!(matches!(
            block.tool_use_result.as_ref(),
            Some(serde_json::Value::String(raw))
                if raw == "Error calling tool (Glob): Path contains null bytes"
        ));
        assert_eq!(
            block.content,
            "<tool_use_error>Error calling tool (Glob): Path contains null bytes</tool_use_error>"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn grep_validation_errors_keep_string_tool_use_results_before_permission() {
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-validation-executor-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root.clone()),
            ..crate::tool::ToolUseContext::default()
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut queue = Vec::new();

        let tolerant = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-grep-semantic-literals".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({
                "pattern": "x",
                "head_limit": "2.5",
                "-i": "true"
            }),
        };
        let tolerant_update = run_tool_use(&tolerant, &assistant, &context, &mut queue);
        assert!(tolerant_update.message.is_none());
        assert_eq!(
            tolerant_update
                .request
                .as_ref()
                .map(|request| &request.input),
            Some(&serde_json::json!({
                "pattern": "x",
                "head_limit": 2.5,
                "-i": true
            }))
        );
        queue.clear();

        let semantic = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-grep-validation".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({"pattern": "x", "path": "missing"}),
        };
        let update = run_tool_use(&semantic, &assistant, &context, &mut queue);
        assert!(!update.blocked_on_permission);
        assert!(queue.is_empty());
        let block = update
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("tool result block");
        assert_eq!(block.derived_status(), ToolResultStatus::Error);
        // The raw `Error: …` string rides the row, not a display variant.
        assert!(matches!(
            block.tool_use_result.as_ref(),
            Some(serde_json::Value::String(raw))
                if raw.starts_with("Error: Path does not exist: missing.")
        ));
        assert!(
            block
                .content
                .contains(crate::utils::file::FILE_NOT_FOUND_CWD_NOTE)
        );
        assert!(matches!(
            update.tool_result.as_ref().and_then(|message| message.content.first()),
            Some(UserContent::ToolResult(result))
                if matches!(result.tool_use_result.as_ref(), Some(serde_json::Value::String(value))
                    if value.starts_with("Error: Path does not exist: missing."))
        ));

        let schema = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-grep-schema".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({"path": ".", "unexpected": true}),
        };
        let invalid = run_tool_use(&schema, &assistant, &context, &mut queue);
        let Some((tool_use_result, content)) = invalid
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .and_then(|block| match block.tool_use_result.as_ref() {
                Some(serde_json::Value::String(raw)) => Some((raw, &block.content)),
                _ => None,
            })
        else {
            panic!(
                "unexpected Grep schema result: {:?}",
                invalid.message.map(|message| message.kind)
            );
        };
        assert!(
            tool_use_result.starts_with("InputValidationError: [\n"),
            "unexpected raw Grep validation toolUseResult: {tool_use_result:?}"
        );
        assert!(tool_use_result.contains("unrecognized_keys"));
        assert_eq!(
            content,
            "<tool_use_error>InputValidationError: Grep failed due to the following issues:\nThe required parameter `pattern` is missing\nAn unexpected parameter `unexpected` was provided</tool_use_error>"
        );

        let fatal = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-grep-fatal".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({"pattern": "x", "path": "bad\0path"}),
        };
        let fatal = run_tool_use(&fatal, &assistant, &context, &mut queue);
        let block = fatal
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("tool result block");
        assert!(matches!(
            block.tool_use_result.as_ref(),
            Some(serde_json::Value::String(raw))
                if raw == "Error calling tool (Grep): Path contains null bytes"
        ));
        assert_eq!(
            block.content,
            "<tool_use_error>Error calling tool (Grep): Path contains null bytes</tool_use_error>"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn yield_missing_tool_result_blocks_pairs_each_tool_use_with_error_result() {
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_missing".to_string()),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "echo late error"}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        let results = yield_missing_tool_result_blocks(&[assistant], "Interrupted by user");

        assert_eq!(results.len(), 1);
        assert!(matches!(
            user_tool_result_block(&results[0].message),
            Some(block) if block.tool_use_id.0 == "toolu_missing"
                && block.derived_status() == ToolResultStatus::Error
                && block.content == "Interrupted by user"
        ));
        assert!(
            results[0]
                .tool_result
                .content
                .iter()
                .any(|content| matches!(
                    content,
                    UserContent::ToolResult(result)
                        if result.tool_use_id.0 == "toolu_missing"
                            && result.content == "Interrupted by user"
                            && result.is_error
                ))
        );
    }

    #[test]
    fn brief_transcript_result_maps_to_official_model_acknowledgement() {
        let message = RenderableMessage::user_tool_result(
            "brief-result",
            "toolu_brief",
            "Hello **there**",
            false,
        );

        let model = transcript_tool_result_to_model_message(&message, "SendUserMessage")
            .expect("brief result should map to model tool_result");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_brief"
                    && result.content == "Message delivered to user."
                    && !result.is_error
        ));
    }

    #[test]
    fn empty_transcript_result_maps_to_official_no_output_marker() {
        let message =
            RenderableMessage::user_tool_result("empty-result", "toolu_empty", "   \n\t", false);

        let model = transcript_tool_result_to_model_message(&message, "Bash")
            .expect("empty result should map to model tool_result");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_empty"
                    && result.content == "(Bash completed with no output)"
                    && !result.is_error
        ));
    }

    #[test]
    fn file_tool_model_message_preserves_official_typed_tool_use_result() {
        // The raw `toolUseResult` rides the row via the trait projection.
        let raw = crate::tools::file_edit_tool::ui::output_to_value(
            &crate::tools::file_edit_tool::types::FileEditOutput {
                file_path: "/tmp/a.rs".to_string(),
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                original_file: "old".to_string(),
                structured_patch: vec![crate::types::message::StructuredDiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec!["-old".to_string(), "+new".to_string()],
                }],
                user_modified: false,
                replace_all: false,
                git_diff: None,
                updated_file: String::new(),
                read_timestamp_ms: 0,
                dynamic_skill_dirs: Vec::new(),
            },
        );
        let message = RenderableMessage::user_tool_result(
            "edit-result",
            "toolu_edit",
            "The file /tmp/a.rs has been updated successfully.",
            false,
        )
        .with_tool_use_result(Some(raw));

        let model =
            transcript_tool_result_to_model_message(&message, "Edit").expect("model result");
        let UserContent::ToolResult(result) = &model.content[0] else {
            panic!("expected tool result")
        };
        let tool_use_result = result
            .tool_use_result
            .as_ref()
            .expect("typed toolUseResult");
        assert_eq!(tool_use_result["filePath"], "/tmp/a.rs");
        assert_eq!(tool_use_result["oldString"], "old");
        assert_eq!(tool_use_result["newString"], "new");
        assert_eq!(tool_use_result["originalFile"], "old");
        assert_eq!(tool_use_result["userModified"], false);
        assert_eq!(tool_use_result["replaceAll"], false);
        assert_eq!(tool_use_result["structuredPatch"][0]["oldStart"], 1);
        assert_eq!(tool_use_result["structuredPatch"][0]["lines"][1], "+new");
        assert_eq!(
            result.content,
            "The file /tmp/a.rs has been updated successfully."
        );
    }

    #[test]
    fn write_tool_use_result_preserves_original_file_and_remote_git_diff_without_model_injection() {
        // The raw `toolUseResult` rides the row via the trait projection.
        let raw = crate::tools::file_write_tool::ui::output_to_value(
            &crate::tools::file_write_tool::WriteOutput {
                kind: crate::tools::file_write_tool::WriteOutputKind::Update,
                file_path: "src/a.rs".to_string(),
                content: "new".to_string(),
                structured_patch: Vec::new(),
                original_file: Some("old".to_string()),
                read_timestamp_ms: 0,
                git_diff: Some(crate::utils::git_diff::ToolUseDiff {
                    filename: "src/a.rs".to_string(),
                    status: crate::utils::git_diff::ToolUseDiffStatus::Modified,
                    additions: 1,
                    deletions: 1,
                    changes: 2,
                    patch: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
                    repository: Some("owner/repository".to_string()),
                }),
                dynamic_skill_dirs: Vec::new(),
            },
        );
        let message = RenderableMessage::user_tool_result(
            "write-result",
            "toolu_write",
            "The file src/a.rs has been updated successfully.",
            false,
        )
        .with_tool_use_result(Some(raw));

        let model =
            transcript_tool_result_to_model_message(&message, "Write").expect("model result");
        let UserContent::ToolResult(result) = &model.content[0] else {
            panic!("expected tool result")
        };
        assert_eq!(
            result.content,
            "The file src/a.rs has been updated successfully."
        );
        let tool_use_result = result
            .tool_use_result
            .as_ref()
            .expect("typed toolUseResult");
        assert_eq!(tool_use_result["originalFile"], "old");
        assert_eq!(tool_use_result["gitDiff"]["status"], "modified");
        assert_eq!(tool_use_result["gitDiff"]["repository"], "owner/repository");
        assert!(!result.content.contains("owner/repository"));
    }

    #[test]
    fn notebook_edit_tool_use_result_preserves_full_official_output_shape() {
        // The raw `toolUseResult` rides the row via the trait projection.
        let raw = crate::tools::notebook_edit_tool::ui::output_to_value(
            &crate::tools::notebook_edit_tool::Output {
                cell_id: Some("cell-a".to_string()),
                new_source: "new".to_string(),
                cell_type: "code".to_string(),
                language: "python".to_string(),
                edit_mode: "replace".to_string(),
                error: None,
                notebook_path: "/tmp/demo.ipynb".to_string(),
                original_file: "{\"source\":\"old\"}".to_string(),
                updated_file: "{\"source\":\"new\"}".to_string(),
                read_timestamp_ms: None,
            },
        );
        let message = RenderableMessage::user_tool_result(
            "notebook-result",
            "toolu_notebook",
            "Updated cell cell-a with new",
            false,
        )
        .with_tool_use_result(Some(raw));

        let model = transcript_tool_result_to_model_message(&message, "NotebookEdit")
            .expect("model result");
        let UserContent::ToolResult(result) = &model.content[0] else {
            panic!("expected tool result")
        };
        let tool_use_result = result
            .tool_use_result
            .as_ref()
            .expect("NotebookEdit toolUseResult");
        assert_eq!(tool_use_result["new_source"], "new");
        assert_eq!(tool_use_result["cell_id"], "cell-a");
        assert_eq!(tool_use_result["cell_type"], "code");
        assert_eq!(tool_use_result["language"], "python");
        assert_eq!(tool_use_result["edit_mode"], "replace");
        assert_eq!(tool_use_result["error"], "");
        assert_eq!(tool_use_result["notebook_path"], "/tmp/demo.ipynb");
        assert_eq!(tool_use_result["original_file"], "{\"source\":\"old\"}");
        assert_eq!(tool_use_result["updated_file"], "{\"source\":\"new\"}");
        assert_eq!(
            tool_use_result
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "new_source",
                "cell_type",
                "language",
                "edit_mode",
                "cell_id",
                "error",
                "notebook_path",
                "original_file",
                "updated_file",
            ]
        );
        assert_eq!(result.content, "Updated cell cell-a with new");
    }

    #[test]
    fn accept_feedback_appends_text_block_after_tool_result_for_model() {
        let message =
            RenderableMessage::user_tool_result("feedback-result", "toolu_feedback", "ok", false);
        let model = append_accept_feedback_to_tool_result(
            transcript_tool_result_to_model_message(&message, "Bash"),
            &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
                .with_feedback("  continue with these results  "),
            PermissionPromptChoice::AllowOnce,
        )
        .expect("model tool_result should exist");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_feedback" && result.content == "ok"
        ));
        assert!(matches!(
            model.content.get(1),
            Some(UserContent::Text(text)) if text == "continue with these results"
        ));
    }

    #[test]
    fn permission_images_are_top_level_after_tool_result_and_accept_feedback() {
        let message = RenderableMessage::user_tool_result(
            "permission-image-result",
            "toolu_permission_image",
            "answers recorded",
            false,
        );
        let response = PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
            .with_feedback("use this context")
            .with_content_blocks(vec![
                crate::types::permissions::PermissionContentBlock::image_base64(
                    "image/png",
                    "AAAA",
                ),
            ]);
        let mut model = append_accept_feedback_to_tool_result(
            transcript_tool_result_to_model_message(&message, "AskUserQuestion"),
            &response,
            PermissionPromptChoice::AllowOnce,
        );
        append_permission_content_blocks(&mut model, &response);
        let model = model.expect("model tool_result should exist");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(_))
        ));
        assert!(matches!(
            model.content.get(1),
            Some(UserContent::Text(text)) if text == "use this context"
        ));
        assert!(matches!(
            model.content.get(2),
            Some(UserContent::Image { media_type, data })
                if media_type == "image/png" && data == "AAAA"
        ));
    }

    #[test]
    fn rejected_permission_images_remain_top_level_after_error_tool_result() {
        let request = mock_permission_request(
            "perm-image-reject".to_string(),
            "toolu_permission_image_reject".to_string(),
            "AskUserQuestion".to_string(),
            "question".to_string(),
            PermissionMode::Default,
        );
        let message = permission_terminal_result_with_feedback(
            &request,
            ToolResultStatus::Rejected,
            Some("change the plan"),
            false,
        );
        let response = PermissionPromptResponse::new(PermissionPromptChoice::Deny)
            .with_feedback("change the plan")
            .with_content_blocks(vec![
                crate::types::permissions::PermissionContentBlock::image_base64(
                    "image/jpeg",
                    "BBBB",
                ),
            ]);
        let mut model = transcript_tool_result_to_model_message(&message, &request.tool_name);
        append_permission_content_blocks(&mut model, &response);
        let model = model.expect("rejected model tool_result should exist");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result)) if result.is_error
        ));
        assert!(matches!(
            model.content.get(1),
            Some(UserContent::Image { media_type, data })
                if media_type == "image/jpeg" && data == "BBBB"
        ));
    }

    #[test]
    fn send_message_transcript_result_preserves_official_json_payload_for_model() {
        let content = serde_json::json!({
            "success": true,
            "message": "Message sent to alice's inbox",
            "routing": {"target": "alice"}
        })
        .to_string();
        let message = RenderableMessage::user_tool_result(
            "send-message-result",
            "toolu_send_message",
            content.clone(),
            false,
        );

        let model = transcript_tool_result_to_model_message(&message, "SendMessage")
            .expect("SendMessage result should map to model tool_result");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_send_message"
                    && result.content == content
                    && !result.is_error
        ));
    }

    #[test]
    fn web_tool_transcript_results_map_to_official_model_content() {
        let fetch_content = serde_json::json!({
            "bytes": 100,
            "code": 200,
            "codeText": "OK",
            "result": "Fetched page summary",
            "durationMs": 1,
            "url": "https://example.com"
        })
        .to_string();
        let fetch_message = RenderableMessage::user_tool_result(
            "webfetch-result",
            "toolu_webfetch",
            fetch_content,
            false,
        );
        let fetch_model = transcript_tool_result_to_model_message(&fetch_message, "WebFetch")
            .expect("WebFetch result should map to model tool_result");
        assert!(matches!(
            fetch_model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_webfetch"
                    && result.content == "Fetched page summary"
                    && !result.is_error
        ));

        let search_content = serde_json::json!({
            "query": "rust ownership",
            "results": [
                "Summary text",
                {"tool_use_id": "srvu_1", "content": [
                    {"title": "Rust Book", "url": "https://doc.rust-lang.org/book/"}
                ]}
            ],
            "durationSeconds": 0.5
        })
        .to_string();
        let search_message = RenderableMessage::user_tool_result(
            "websearch-result",
            "toolu_websearch",
            search_content,
            false,
        );
        let search_model = transcript_tool_result_to_model_message(&search_message, "WebSearch")
            .expect("WebSearch result should map to model tool_result");
        assert!(matches!(
            search_model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_websearch"
                    && result.content.contains("Web search results for query: \"rust ownership\"")
                    && result.content.contains("Summary text")
                    && result.content.contains("Links: [{\"title\":\"Rust Book\",\"url\":\"https://doc.rust-lang.org/book/\"}]")
                    && result.content.contains("REMINDER: You MUST include the sources")
                    && !result.is_error
        ));
    }

    #[test]
    fn worktree_transcript_result_maps_official_message_to_model() {
        let content = serde_json::json!({
            "worktreePath": "/tmp/worktree",
            "worktreeBranch": "feature",
            "message": "Created worktree at /tmp/worktree on branch feature."
        })
        .to_string();
        let message = RenderableMessage::user_tool_result(
            "worktree-result",
            "toolu_worktree",
            content,
            false,
        );

        let model = transcript_tool_result_to_model_message(&message, "EnterWorktree")
            .expect("worktree result should map to model tool_result");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_worktree"
                    && result.content == "Created worktree at /tmp/worktree on branch feature."
                    && !result.is_error
        ));
    }

    #[test]
    fn ask_user_question_transcript_result_maps_to_official_model_answer_summary() {
        let content = serde_json::json!({
            "questions": [],
            "answers": {"Proceed?": "Yes"},
            "annotations": {"Proceed?": {"notes": "Looks good"}},
        })
        .to_string();
        let message =
            RenderableMessage::user_tool_result("ask-result", "toolu_ask", content, false);

        let model = transcript_tool_result_to_model_message(&message, "AskUserQuestion")
            .expect("AskUserQuestion result should map to model tool_result");

        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_id.0 == "toolu_ask"
                    && result.content.contains("User has answered your questions")
                    && result.content.contains("\"Proceed?\"=\"Yes\"")
                    && result.content.contains("user notes: Looks good")
                    && !result.is_error
        ));
    }

    #[test]
    fn tool_search_transcript_result_maps_matches_to_tool_reference_blocks() {
        let content = serde_json::json!({
            "matches": ["Read", "Grep"],
            "query": "select:Read,Grep",
            "total_deferred_tools": 2,
        })
        .to_string();
        let message = RenderableMessage::user_tool_result(
            "tool-search-result",
            "toolu_tool_search",
            content.clone(),
            false,
        );

        let model = transcript_tool_result_to_model_message(&message, "ToolSearch")
            .expect("ToolSearch result should map to model tool_result");

        match model.content.first() {
            Some(UserContent::ToolResult(result)) => {
                assert_eq!(result.tool_use_id.0, "toolu_tool_search");
                assert_eq!(result.content, content);
                assert!(!result.is_error);
                assert_eq!(result.content_blocks.len(), 2);
                assert!(matches!(
                    &result.content_blocks[0],
                    crate::types::message::ToolResultContentBlock::ToolReference { tool_name }
                        if tool_name == "Read"
                ));
            }
            other => panic!("unexpected ToolSearch model content: {other:?}"),
        }
    }

    #[test]
    fn tool_search_transcript_result_reports_pending_mcp_servers_on_no_match() {
        let content = serde_json::json!({
            "matches": [],
            "query": "slack",
            "total_deferred_tools": 0,
            "pending_mcp_servers": ["slack", "github"]
        })
        .to_string();
        let message = RenderableMessage::user_tool_result(
            "tool-search-result",
            "toolu_tool_search",
            content,
            false,
        );

        let model = transcript_tool_result_to_model_message(&message, "ToolSearch")
            .expect("ToolSearch result should map to model tool_result");

        match model.content.first() {
            Some(UserContent::ToolResult(result)) => {
                assert_eq!(result.tool_use_id.0, "toolu_tool_search");
                assert_eq!(
                    result.content,
                    "No matching deferred tools found. Some MCP servers are still connecting: slack, github. Their tools will become available shortly — try searching again."
                );
                assert!(result.content_blocks.is_empty());
            }
            other => panic!("unexpected ToolSearch model content: {other:?}"),
        }
    }

    #[test]
    fn queued_tool_use_creates_permission_request() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);

        assert!(update.blocked_on_permission);
        assert_eq!(queue[0].tool_use_id(), "toolu_mock");
        assert_eq!(queue[0].request.tool_name, "Bash");
        assert_eq!(queue[0].request.input_summary, "printf permission-gated");
    }

    #[test]
    fn glob_outside_cwd_queues_read_permission_instead_of_auto_rejecting() {
        let root = std::env::temp_dir().join(format!(
            "cometix-glob-gate-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-glob-gate-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let message = RenderableMessage::assistant_block(
            "toolu_glob_permission",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_glob_permission".to_string()),
                name: "Glob".to_string(),
                input: serde_json::json!({
                    "pattern": "*",
                    "path": outside.display().to_string()
                }),
            }),
        );
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.cwd_override = Some(root.clone());
        let mut queue = Vec::new();
        let update = run_tool_use_permission_gate(&message, &context, &mut queue);

        assert!(update.blocked_on_permission);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].request.tool_name, "Glob");
        assert_eq!(queue[0].request.input["pattern"], "*");
        assert_eq!(
            queue[0].request.input["path"],
            outside.display().to_string()
        );

        queue.clear();
        context.tool_permission_context.mode = PermissionMode::BypassPermissions;
        let bypass = run_tool_use_permission_gate(&message, &context, &mut queue);
        assert!(!bypass.blocked_on_permission);
        assert!(queue.is_empty());
        assert_eq!(bypass.forced_choice, None);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn grep_outside_cwd_prompts_normally_but_bypass_mode_runs_without_queueing() {
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-gate-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = std::env::temp_dir().join(format!(
            "cometix-grep-gate-outside-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let message = RenderableMessage::assistant_block(
            "toolu_grep_permission",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_grep_permission".to_string()),
                name: "Grep".to_string(),
                input: serde_json::json!({
                    "pattern": "needle",
                    "path": outside.display().to_string()
                }),
            }),
        );
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.cwd_override = Some(root.clone());
        let mut queue = Vec::new();
        let normal = run_tool_use_permission_gate(&message, &context, &mut queue);
        assert!(normal.blocked_on_permission);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].request.tool_name, "Grep");

        queue.clear();
        context.tool_permission_context.mode = PermissionMode::BypassPermissions;
        let bypass = run_tool_use_permission_gate(&message, &context, &mut queue);
        assert!(
            !bypass.blocked_on_permission,
            "unexpected Grep bypass queue: {queue:?}"
        );
        assert!(queue.is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn aborted_context_permission_gate_yields_cancelled_tool_result() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.abort_controller.abort();
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(queue.is_empty());
        assert!(update.request.is_none());
        assert!(matches!(
            update.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Canceled
                && block.content == crate::utils::messages::CANCEL_MESSAGE
        ));
        assert!(update.tool_result.is_some());
    }

    /// The row no longer carries a status, so "already finished" is not
    /// expressible here — and it does not need to be. CC gates once inside
    /// `runToolUse`, before the tool starts, so the caller never hands this
    /// seam a finished tool use. What the guard does protect is the case the
    /// live set can answer: a tool already started must not be gated twice.
    #[test]
    fn tool_use_already_in_the_live_set_is_not_gated_twice() {
        let message = RenderableMessage::assistant_block(
            "toolu_running",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_running".to_string()),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cargo check"}),
            }),
        );

        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();
        // Not started yet: the gate runs and asks.
        let fresh = run_tool_use_permission_gate(&message, &context, &mut queue);
        assert!(fresh.blocked_on_permission || !queue.is_empty());

        // Maps to CC `Tool.ts:227` — execution added it to the REPL-owned set.
        context
            .in_progress_tool_use_ids
            .insert("toolu_running".to_string());
        let mut queue = Vec::new();
        let running = run_tool_use_permission_gate(&message, &context, &mut queue);

        assert!(!running.blocked_on_permission);
        assert!(queue.is_empty());
    }

    #[test]
    fn read_permission_request_backfills_observable_path_and_keeps_private_call_input() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_structured".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({
                "file_path": "src/main.rs",
                "offset": 2,
                "limit": 3
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        let request = update.request.expect("permission request should exist");
        assert_eq!(
            request.input["file_path"],
            context
                .effective_cwd()
                .join("src/main.rs")
                .display()
                .to_string()
        );
        assert_eq!(request.input["offset"], 2);
        assert_eq!(request.input["limit"], 3);
        assert_eq!(
            request.call_input.as_ref().unwrap()["file_path"],
            "src/main.rs"
        );
    }

    #[test]
    fn write_backfills_observable_path_and_keeps_private_call_input() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_write_backfill".to_string()),
            name: "Write".to_string(),
            input: serde_json::json!({
                "file_path": "nested/new.txt",
                "content": "hello"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let root = std::env::temp_dir().join("cometix-write-observable-root");
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.cwd_override = Some(root.clone());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        assert_eq!(
            request.input["file_path"],
            root.join("nested/new.txt").display().to_string()
        );
        assert_eq!(
            request.call_input.as_ref().unwrap()["file_path"],
            "nested/new.txt"
        );
        assert_eq!(
            request.call_input.as_ref().unwrap()
                [crate::tools::file_write_tool::CHECKED_WRITE_DESTINATION_KEY],
            crate::tools::file_write_tool::resolved_write_destination(
                &root.join("nested/new.txt"),
            )
            .display()
            .to_string()
        );
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .to_string()
                .find(crate::tools::file_write_tool::CHECKED_WRITE_DESTINATION_KEY)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_permission_transport_rejects_retargeted_dangling_symlink() {
        use std::os::unix::fs::symlink;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-write-retarget-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let link = root.join("approved-link.txt");
        let approved_target = root.join("approved-target.txt");
        let swapped_target = root.join("swapped-target.txt");
        symlink(&approved_target, &link).unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_write_retarget".to_string()),
            name: "Write".to_string(),
            input: serde_json::json!({
                "file_path": link.display().to_string(),
                "content": "must not land"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("permission request");
        assert_eq!(
            request.call_input.as_ref().unwrap()
                [crate::tools::file_write_tool::CHECKED_WRITE_DESTINATION_KEY],
            approved_target.display().to_string()
        );

        std::fs::remove_file(&link).unwrap();
        symlink(&swapped_target, &link).unwrap();
        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
        )
        .await;
        assert!(!approved_target.exists());
        assert!(!swapped_target.exists());
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content.contains("unexpectedly modified")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_permission_transport_pins_symlink_destination() {
        use std::os::unix::fs::symlink;

        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-pinned-destination-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let approved_target = root.join("approved.txt");
        let swapped_target = root.join("swapped.txt");
        let link = root.join("logical.txt");
        std::fs::write(&approved_target, "old\n").unwrap();
        std::fs::write(&swapped_target, "old\n").unwrap();
        symlink(&approved_target, &link).unwrap();

        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-edit-pinned".to_string()),
            name: "Edit".to_string(),
            input: serde_json::json!({
                "file_path": link.display().to_string(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.cwd_override = Some(root.clone());
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: link.display().to_string(),
                content: Some("old\n".to_string()),
                timestamp_ms: crate::utils::file::get_file_modification_time(&link),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("Edit permission request");
        assert_eq!(
            request.call_input.as_ref().unwrap()
                [crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY],
            approved_target
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );

        std::fs::remove_file(&link).unwrap();
        symlink(&swapped_target, &link).unwrap();
        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&approved_target).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&swapped_target).unwrap(), "old\n");
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content.contains("unexpectedly modified")
        ));
        assert!(matches!(
            result
                .tool_result
                .as_ref()
                .and_then(|message| message.content.first()),
            Some(UserContent::ToolResult(block))
                if block.tool_use_result.as_ref().is_some_and(|raw| {
                    raw.as_str().is_some_and(|text| {
                        text.starts_with("Error: File has been unexpectedly modified")
                    })
                })
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn notebook_edit_permission_transport_rejects_retargeted_symlink() {
        use std::os::unix::fs::symlink;

        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-notebook-pinned-destination-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let approved_target = root.join("approved.ipynb");
        let swapped_target = root.join("swapped.ipynb");
        let link = root.join("logical.ipynb");
        let notebook = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [{
                "cell_type": "code",
                "id": "cell-a",
                "source": "old",
                "metadata": {},
                "execution_count": null,
                "outputs": []
            }]
        })
        .to_string();
        std::fs::write(&approved_target, &notebook).unwrap();
        std::fs::write(&swapped_target, &notebook).unwrap();
        symlink(&approved_target, &link).unwrap();

        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-notebook-pinned".to_string()),
            name: "NotebookEdit".to_string(),
            input: serde_json::json!({
                "notebook_path": link.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "must not land"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.cwd_override = Some(root.clone());
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: link.display().to_string(),
                content: Some(notebook.clone()),
                timestamp_ms: crate::utils::file::get_file_modification_time(&link),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let mut request = update
            .request
            .unwrap_or_else(|| panic!("NotebookEdit permission request: {:?}", update.message));
        assert_eq!(
            request.call_input.as_ref().unwrap()
                [crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY],
            approved_target
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .to_string()
                .find(crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY)
                .is_none()
        );

        std::fs::remove_file(&link).unwrap();
        symlink(&swapped_target, &link).unwrap();
        // A same-path continuation must preserve the pre-prompt destination;
        // only an authoritative logical path rewrite may replace it.
        pin_user_approved_edit_destination(&mut request, &context);
        assert_eq!(
            request.call_input.as_ref().unwrap()
                [crate::tools::notebook_edit_tool::CHECKED_NOTEBOOK_DESTINATION_KEY],
            approved_target
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&approved_target).unwrap(), notebook);
        assert_eq!(std::fs::read_to_string(&swapped_target).unwrap(), notebook);
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content.contains("unexpectedly modified")
        ));
        assert!(matches!(
            result
                .tool_result
                .as_ref()
                .and_then(|message| message.content.first()),
            Some(UserContent::ToolResult(block))
                if block.tool_use_result.as_ref().is_some_and(|raw| {
                    raw["error"].as_str().is_some_and(|text| {
                        text.starts_with("File has been unexpectedly modified")
                    })
                })
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_equivalence_io_error_propagates_before_pre_tool_hooks() {
        use std::os::unix::fs::symlink;

        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-equivalence-io-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("loop");
        symlink(&path, &path).unwrap();
        let marker = root.join("hook-ran");
        let request = mock_permission_request_with_input(
            "perm-edit-equivalence-io",
            "toolu-edit-equivalence-io",
            "Edit",
            path.display().to_string(),
            serde_json::json!({
                "file_path": path.display().to_string(),
                "old_string": "old",
                "new_string": "model"
            }),
            PermissionMode::Default,
        );
        let response = PermissionPromptResponse::allow_once_with_input(serde_json::json!({
            "file_path": path.display().to_string(),
            "old_string": "old",
            "new_string": "user"
        }));
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Edit",
                "hooks": [{
                    "command": format!("touch '{}'", marker.display()),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let result = check_permissions_and_call_tool_with_config_inner(
            &request,
            response,
            &ToolUseContext::default(),
            None,
            &config,
            Vec::new(),
            true,
            false,
            None,
        )
        .await;
        assert!(!marker.exists());
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn edit_no_write_preflight_skips_production_pre_tool_hook_side_effects() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-disabled-hook-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("hook-ran");
        let target = root.join("target.txt");
        let request = mock_permission_request_with_input(
            "perm-edit-disabled-hook",
            "toolu-edit-disabled-hook",
            "Edit",
            target.display().to_string(),
            serde_json::json!({
                "file_path": target.display().to_string(),
                "old_string": "",
                "new_string": "blocked"
            }),
            PermissionMode::Default,
        );
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Edit",
                "hooks": [{
                    "command": format!("touch '{}'", marker.display()),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &ToolUseContext::default(),
            None,
            &config,
            Vec::new(),
        )
        .await;
        assert!(!marker.exists());
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn edit_hook_path_rewrite_fails_closed_against_user_approved_destination() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-hook-path-pin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let approved = root.join("approved.txt");
        let rewritten = root.join("rewritten.txt");
        std::fs::write(&approved, "old\n").unwrap();
        std::fs::write(&rewritten, "old\n").unwrap();
        let request = mock_permission_request_with_input(
            "perm-edit-hook-pin",
            "toolu-edit-hook-pin",
            "Edit",
            approved.display().to_string(),
            serde_json::json!({
                "file_path": approved.display().to_string(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
            PermissionMode::Default,
        );
        let hook_payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {
                    "file_path": rewritten.display().to_string(),
                    "old_string": "old",
                    "new_string": "new",
                    "replace_all": false
                }
            }
        })
        .to_string();
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Edit",
                "hooks": [{
                    "command": format!("printf '%s' '{}'", hook_payload),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let mut context = ToolUseContext::default();
        for path in [&approved, &rewritten] {
            context
                .read_file_state
                .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                    path: path.display().to_string(),
                    content: Some("old\n".to_string()),
                    timestamp_ms: crate::utils::file::get_file_modification_time(path),
                    offset: None,
                    limit: None,
                    is_partial_view: false,
                    source: crate::utils::query_helpers::ReadFileStateSource::Read,
                });
        }
        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            &config,
            Vec::new(),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&approved).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&rewritten).unwrap(), "old\n");
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content.contains("unexpectedly modified")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn edit_execution_projects_dynamic_skill_and_read_state_effects() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-dynamic-context-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("packages/pkg/.claude/skills");
        std::fs::create_dir_all(skill_dir.join("nested")).unwrap();
        std::fs::write(
            skill_dir.join("nested/SKILL.md"),
            "---\nname: nested\ndescription: Nested\n---\nBody",
        )
        .unwrap();
        let target = root.join("packages/pkg/src/value.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "old\n").unwrap();
        let request = mock_permission_request_with_input(
            "perm-edit-dynamic",
            "toolu-edit-dynamic",
            "Edit",
            target.display().to_string(),
            serde_json::json!({
                "file_path": target.display().to_string(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
            PermissionMode::AcceptEdits,
        );
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        });
        context.cwd_override = Some(root.clone());
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: target.display().to_string(),
                content: Some("old\n".to_string()),
                timestamp_ms: crate::utils::file::get_file_modification_time(&target),
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });

        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;
        assert!(
            result
                .new_context
                .dynamic_skill_dir_triggers
                .as_ref()
                .is_some_and(|triggers| triggers.contains(&skill_dir.display().to_string()))
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        let state = result
            .new_context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| entry.path == target.display().to_string())
            .expect("Edit updates readFileState");
        assert_eq!(state.content.as_deref(), Some("new\n"));
        assert_eq!(
            state.timestamp_ms,
            crate::utils::file::get_file_modification_time(&target)
        );

        crate::skills::load_skills_dir::clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn write_execution_projects_dynamic_skill_dir_effect_into_next_context() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-write-dynamic-context-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("packages/pkg/.claude/skills");
        std::fs::create_dir_all(skill_dir.join("nested")).unwrap();
        std::fs::write(
            skill_dir.join("nested/SKILL.md"),
            "---\nname: nested\ndescription: Nested\n---\nBody",
        )
        .unwrap();
        let target = root.join("packages/pkg/src/new.txt");
        let mut request = mock_permission_request_with_input(
            "perm-write-dynamic",
            "toolu-write-dynamic",
            "Write",
            target.display().to_string(),
            serde_json::json!({
                "file_path": target.display().to_string(),
                "content": "hello"
            }),
            PermissionMode::AcceptEdits,
        );
        request.call_input = Some(serde_json::json!({
            "file_path": "packages/pkg/src/new.txt",
            "content": "hello"
        }));
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        });
        context.cwd_override = Some(root.clone());

        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;
        assert!(
            result
                .new_context
                .dynamic_skill_dir_triggers
                .as_ref()
                .is_some_and(|triggers| triggers.contains(&skill_dir.display().to_string()))
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        let state = result
            .new_context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| entry.path == target.display().to_string())
            .expect("Write updates readFileState");
        assert_eq!(state.content.as_deref(), Some("hello"));
        assert_eq!(
            state.timestamp_ms,
            crate::utils::file::get_file_modification_time(&target)
        );

        crate::skills::load_skills_dir::clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn failed_write_after_discovery_still_projects_dynamic_skill_trigger() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-write-failed-dynamic-context-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill_dir = root.join("packages/pkg/.claude/skills");
        std::fs::create_dir_all(skill_dir.join("nested")).unwrap();
        std::fs::write(
            skill_dir.join("nested/SKILL.md"),
            "---\nname: nested\ndescription: Nested\n---\nBody",
        )
        .unwrap();
        let target = root.join("packages/pkg/src/existing.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "old").unwrap();
        let old_timestamp = crate::utils::file::get_file_modification_time(&target);
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&target, "raced").unwrap();
        let request = mock_permission_request_with_input(
            "perm-write-failed-dynamic",
            "toolu-write-failed-dynamic",
            "Write",
            target.display().to_string(),
            serde_json::json!({
                "file_path": target.display().to_string(),
                "content": "new"
            }),
            PermissionMode::AcceptEdits,
        );
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext {
            mode: PermissionMode::AcceptEdits,
            ..ToolPermissionContext::default()
        });
        context.cwd_override = Some(root.clone());
        context
            .read_file_state
            .set_entry(crate::utils::query_helpers::ReadFileStateEntry {
                path: target.display().to_string(),
                content: Some("old".to_string()),
                timestamp_ms: old_timestamp,
                offset: None,
                limit: None,
                is_partial_view: false,
                source: crate::utils::query_helpers::ReadFileStateSource::Read,
            });

        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;
        assert!(
            result
                .new_context
                .dynamic_skill_dir_triggers
                .as_ref()
                .is_some_and(|triggers| triggers.contains(&skill_dir.display().to_string()))
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "raced");
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
        ));

        crate::skills::load_skills_dir::clear_dynamic_skills();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_tool_use_threads_semantically_parsed_input_through_permission_request() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_semantic_numbers".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({
                "file_path": "src/main.rs",
                "offset": "2",
                "limit": "3"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        assert!(update.tool_result.is_none());
        let request = update.request.expect("permission request should exist");
        assert_eq!(request.input["offset"], serde_json::json!(2));
        assert_eq!(request.input["limit"], serde_json::json!(3));
    }

    #[test]
    fn run_tool_use_invalid_input_returns_error_tool_result_without_permission() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_invalid_bash".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "timeout": 1000 }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(update.request.is_none());
        assert!(queue.is_empty());
        let tool_result = update
            .tool_result
            .expect("invalid input should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_invalid_bash");
                assert!(result.is_error);
                assert!(result.content.contains("InputValidationError"));
                assert!(result.content.contains("missing required field `command`"));
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[test]
    fn run_tool_use_deferred_schema_validation_adds_tool_search_hint_when_schema_was_not_sent() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _disable_betas_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let _tool_search_guard = EnvVarGuard::unset("ENABLE_TOOL_SEARCH");
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_invalid_web_fetch".to_string()),
            name: "WebFetch".to_string(),
            input: serde_json::json!({
                "url": ["https://example.com"],
                "prompt": "summarize"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_tools(vec![
                crate::tools::web_fetch_tool::web_fetch_tool_schema(),
                crate::tools::tool_search_tool::tool_search_tool_schema(),
            ]);
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        assert!(!update.blocked_on_permission);
        let tool_result = update
            .tool_result
            .expect("invalid deferred input should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert!(result.is_error);
                assert!(result.content.contains("InputValidationError"));
                assert!(result.content.contains("schema was not sent to the API"));
                assert!(result.content.contains("select:WebFetch"));
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[test]
    fn run_tool_use_deferred_schema_validation_skips_tool_search_hint_after_discovery() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _disable_betas_guard = EnvVarGuard::unset("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS");
        let _tool_search_guard = EnvVarGuard::unset("ENABLE_TOOL_SEARCH");
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_invalid_web_fetch_discovered".to_string()),
            name: "WebFetch".to_string(),
            input: serde_json::json!({
                "url": ["https://example.com"],
                "prompt": "summarize"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let discovered_message = Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(crate::types::message::ToolResult {
                tool_use_id: crate::types::ids::ToolUseId("toolu_tool_search".to_string()),
                content: "{}".to_string(),
                is_error: false,
                content_blocks: vec![
                    crate::types::message::ToolResultContentBlock::ToolReference {
                        tool_name: "WebFetch".to_string(),
                    },
                ],
                tool_use_result: None,
            })],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        });
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_tools(vec![
                crate::tools::web_fetch_tool::web_fetch_tool_schema(),
                crate::tools::tool_search_tool::tool_search_tool_schema(),
            ])
            .with_messages(vec![discovered_message]);
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        let tool_result = update
            .tool_result
            .expect("invalid deferred input should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert!(result.is_error);
                assert!(result.content.contains("InputValidationError"));
                assert!(!result.content.contains("schema was not sent to the API"));
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[test]
    fn schema_validator_enforces_combinators_patterns_and_numeric_bounds() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "pattern": "^[a-z]+$"},
                "timeout": {"type": "integer", "minimum": 0, "maximum": 10},
                "payload": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "x"}}, "additionalProperties": false}
                    ]
                }
            },
            "required": ["id", "timeout", "payload"],
            "additionalProperties": false
        });
        assert!(
            validate_tool_input_against_schema(
                "Example",
                &serde_json::json!({"id": "abc", "timeout": 10, "payload": {"kind": "x"}}),
                &schema,
            )
            .is_ok()
        );
        assert!(
            validate_tool_input_against_schema(
                "Example",
                &serde_json::json!({"id": "ABC", "timeout": 11, "payload": {"kind": "y"}}),
                &schema,
            )
            .is_err()
        );
    }

    #[test]
    fn structured_output_uses_full_draft7_runtime_validation() {
        let schema = serde_json::json!({
            "type": "object",
            "definitions": {
                "positiveCount": {"type": "integer", "minimum": 1}
            },
            "properties": {
                "kind": {"enum": ["counted"]},
                "count": {"$ref": "#/definitions/positiveCount"}
            },
            "required": ["kind", "count"],
            "dependencies": {"kind": ["count"]},
            "additionalProperties": false
        });
        assert!(
            validate_tool_input_against_schema(
                crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
                &serde_json::json!({"kind": "counted", "count": 2}),
                &schema,
            )
            .is_ok()
        );
        let error = validate_tool_input_against_schema(
            crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
            &serde_json::json!({"kind": "counted", "count": 0}),
            &schema,
        )
        .unwrap_err();
        // CC `SyntheticOutputTool.ts:145-151`.
        assert!(
            error.starts_with("Output does not match required schema: "),
            "unexpected StructuredOutput validation error: {error}"
        );
        assert!(error.contains("/count: "));
    }

    /// CC `toolExecution.ts:1272-1279`: a tool result carrying
    /// `structured_output` also lands in the stream as a `structured_output`
    /// attachment message (transcript-visible), not only in the context slot.
    #[test]
    fn structured_output_result_also_lands_as_attachment_message() {
        let mut new_messages: Vec<Message> = Vec::new();
        let output = serde_json::json!({"ok": true, "count": 2});
        super::push_structured_output_attachment(&mut new_messages, &output);
        let [Message::Attachment(attachment)] = new_messages.as_slice() else {
            panic!("expected exactly one attachment message: {new_messages:?}");
        };
        assert_eq!(attachment.attachment_type(), "structured_output");
        assert!(matches!(
            &attachment.attachment,
            crate::types::message::Attachment::StructuredOutput { data } if *data == output
        ));
    }

    #[test]
    fn run_tool_use_nested_schema_validation_rejects_invalid_ask_user_question() {
        // `ToolUseContext::with_permission_context` resolves the live tool pool,
        // which sibling tests narrow through `CLAUDE_CODE_SIMPLE`.
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_invalid_ask".to_string()),
            name: "AskUserQuestion".to_string(),
            input: serde_json::json!({ "questions": [] }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(update.request.is_none());
        assert!(queue.is_empty());
        let tool_result = update
            .tool_result
            .expect("invalid AskUserQuestion input should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_invalid_ask");
                assert!(result.is_error);
                assert!(
                    result.content.contains("InputValidationError"),
                    "unexpected AskUserQuestion validation content: {}",
                    result.content
                );
                assert!(result.content.contains("questions"));
                assert!(result.content.contains("at least 1"));
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[test]
    fn run_tool_use_unknown_tool_returns_error_tool_result_without_permission() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_unknown".to_string()),
            name: "DefinitelyMissingTool".to_string(),
            input: serde_json::json!({"value": 1}),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(update.request.is_none());
        assert!(queue.is_empty());
        assert!(update.message.is_some());
        let tool_result = update
            .tool_result
            .expect("unknown tool should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_unknown");
                assert!(result.is_error);
                assert!(
                    result
                        .content
                        .contains("No such tool available: DefinitelyMissingTool")
                );
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[tokio::test]
    async fn skill_allowed_tools_widen_a_later_tool_use_in_the_same_turn() {
        // Maps to CC `SkillTool.ts:775-805` (`contextModifier` unions the
        // skill's `allowedTools` into `alwaysAllowRules.command`) read at
        // `toolExecution.ts:1400`, paired at `:1465-1470`, and applied by the
        // orchestration layer to the context threaded into every SUBSEQUENT
        // tool use (`toolOrchestration.ts:140-142`,
        // `StreamingToolExecutor.ts:391-395`).
        //
        // Old shape: `SkillTool::call` returned no modifier and tool execution
        // read none, so `new_context` came back with the pre-skill permission
        // context and the Bash assertion below stayed `true` — the skill's
        // `allowed-tools` reached only its own `!` blocks.
        use std::io::Write;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-skill-context-modifier-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_file = root
            .join(".claude")
            .join("skills")
            .join("release")
            .join("SKILL.md");
        std::fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&skill_file).unwrap();
        // `printf`, not `echo`: `echo` is read-only classified and auto-allowed,
        // so an echo fixture would pass without the frontmatter grant.
        file.write_all(
            b"---\ndescription: Release\nallowed-tools: Bash(printf *)\neffort: high\n---\nRelease body",
        )
        .unwrap();

        let old_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd(&root);

        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.main_loop_model = Some("claude-sonnet-4-5".to_string());

        // The later tool use: a Bash `printf` that the pre-skill context asks about.
        let bash_request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-bash".to_string(),
                "toolu_bash".to_string(),
                crate::tools::bash_tool::tool_name::BASH_TOOL_NAME.to_string(),
                "printf hi".to_string(),
                serde_json::json!({"command": "printf hi"}),
                crate::types::permissions::PermissionMode::Default,
            );
        assert!(
            should_ask_permission_request(&bash_request, &context),
            "pre-skill context must still ask for Bash(printf ...)"
        );

        let skill_request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-skill".to_string(),
                "toolu_skill".to_string(),
                "Skill".to_string(),
                "release".to_string(),
                serde_json::json!({"skill": "release"}),
                crate::types::permissions::PermissionMode::Default,
            );
        let result = streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
            &skill_request,
            &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
            false,
            None,
            &context,
            None,
        )
        .await;

        crate::bootstrap::state::set_original_cwd(old_cwd);
        let _ = std::fs::remove_dir_all(&root);

        let modifier = result
            .context_modifier
            .as_ref()
            .expect("inline skill produces a ToolResult.contextModifier");
        // CC pairs the modifier with the executing tool_use id at `:1465-1470`.
        assert_eq!(modifier.tool_use_id, "toolu_skill");
        assert!(matches!(
            modifier.operation,
            ToolContextModifierOperation::Skill(_)
        ));

        assert!(
            !should_ask_permission_request(&bash_request, &result.new_context),
            "the skill's allowed-tools must widen the NEXT tool use of the turn"
        );
        assert_eq!(
            result.new_context.effort_value,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        // Turn-scoped: the widening lives on the context handed forward, not in
        // the caller's own context (CC never calls `setAppState` here).
        assert!(should_ask_permission_request(&bash_request, &context));
    }

    #[test]
    fn check_permissions_and_call_tool_applies_plan_mode_context_effects() {
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let enter_request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-enter-plan".to_string(),
                "toolu_enter_plan".to_string(),
                "EnterPlanMode".to_string(),
                "{}".to_string(),
                serde_json::json!({}),
                crate::types::permissions::PermissionMode::Default,
            );

        let enter_result = check_permissions_and_call_tool(
            &enter_request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        );
        assert_eq!(
            enter_result.new_context.tool_permission_context.mode,
            crate::types::permissions::PermissionMode::Plan
        );

        let exit_request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-exit-plan".to_string(),
                "toolu_exit_plan".to_string(),
                "ExitPlanMode".to_string(),
                "approve plan".to_string(),
                serde_json::json!({"plan": "## Plan\nShip it"}),
                crate::types::permissions::PermissionMode::Plan,
            );
        let exit_result = check_permissions_and_call_tool(
            &exit_request,
            PermissionPromptChoice::AllowOnce,
            &enter_result.new_context,
            None,
        );
        assert_eq!(
            exit_result.new_context.tool_permission_context.mode,
            crate::types::permissions::PermissionMode::Default
        );
    }

    #[test]
    fn check_permissions_and_call_tool_owns_tool_result_mapping() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");

        let result = check_permissions_and_call_tool(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        );

        assert!(result.message.is_some());
        assert_eq!(
            result.permission_decision.choice,
            PermissionPromptChoice::AllowOnce
        );
        let tool_result = result.tool_result.expect("model tool_result should exist");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_mock");
                assert!(!result.is_error);
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[tokio::test]
    async fn streamed_check_permissions_and_call_tool_with_config_runs_post_tool_hooks() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PostToolUse": [{
                "matcher": "Bash",
                "hooks": [{ "command": "echo post hook ran", "timeout": 5 }]
            }]
        }))
        .unwrap();

        let result = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            Some(&config),
            vec![],
        )
        .await;

        assert!(result.tool_result.is_some());
        assert!(result.pre_tool_messages.is_empty());
        assert!(result.post_tool_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content: text, .. })
                if text.trim() == "post hook ran"
        )));
        assert!(result.hook_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content: text, .. })
                if text.trim() == "post hook ran"
        )));
    }

    /// End-to-end half of #210: a tool whose call FAILS reaches
    /// PostToolUseFailure through the branch `post_tool_hook_event` now owns,
    /// and the payload carries `is_interrupt: false` — CC's
    /// `toolExecution.ts:1694` value for a non-`AbortError` throw, written
    /// unconditionally at `hooks.ts:3516`.
    ///
    /// The interrupted half is NOT asserted here on purpose: with the
    /// controller aborted, `exec_command_hook` refuses to spawn (CC does the
    /// same at `hooks.ts:2015-2017`, where `executeHooks` returns on
    /// `signal?.aborted` before matching), so no capture file exists to read.
    /// `post_tool_hook_event_matches_the_official_try_catch_split` covers the
    /// `is_interrupt: true` classification and
    /// `tool_hooks::tests::post_tool_use_failure_payload_carries_is_interrupt_both_ways`
    /// covers its wire shape.
    ///
    /// Old shape: `run_post_tool_use_failure_hooks` had no flag, so the
    /// captured payload had no `is_interrupt` key and the assertion read
    /// `Null`.
    #[cfg(unix)]
    #[tokio::test]
    async fn failing_tool_fires_post_tool_use_failure_with_is_interrupt_false() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let capture = std::env::temp_dir().join(format!(
            "cometix-post-failure-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let missing = std::env::temp_dir().join(format!(
            "cometix-absent-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));

        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        assert!(
            !context.abort_controller.is_aborted(),
            "precondition: the turn is live, so this is a plain failure"
        );
        let block = RenderableMessage::assistant_block(
            "toolu_read_failure",
            crate::types::message::AssistantContent::ToolUse(ToolUseBlock {
                id: crate::types::ids::ToolUseId("toolu_read_failure".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": missing.display().to_string()}),
            }),
        );
        let mut queue = Vec::new();
        let request = run_tool_use_permission_gate(&block, &context, &mut queue)
            .request
            .or_else(|| queue.pop().map(|confirm| confirm.request))
            .expect("the gate produces a Read permission request");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PostToolUseFailure": [{
                "matcher": "Read",
                "hooks": [{"command": format!("cat > '{}'", capture.display()), "timeout": 10}]
            }],
            // Configured but must NOT run: the call threw, so CC's `catch` owns
            // this tool_use and PostToolUse never sees it.
            "PostToolUse": [{
                "matcher": "Read",
                "hooks": [{"command": "printf 'post-tool must not run' >&2; exit 2", "timeout": 10}]
            }]
        }))
        .unwrap();

        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            &config,
            Vec::new(),
        )
        .await;

        assert_eq!(
            tool_result_status(result.message.as_ref()),
            Some(ToolResultStatus::Error),
            "precondition: reading an absent path fails"
        );
        let payload: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&capture).expect("the PostToolUseFailure hook received its stdin"),
        )
        .expect("the hook input is JSON");
        let _ = std::fs::remove_file(&capture);
        assert_eq!(payload["hook_event_name"], "PostToolUseFailure");
        assert_eq!(payload["tool_use_id"], "toolu_read_failure");
        assert_eq!(
            payload["is_interrupt"],
            serde_json::json!(false),
            "a plain failure sends the key with `false`, not no key: {payload}"
        );
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty()),
            "CC passes `formatError(error)` as `error` (`toolExecution.ts:1691`)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_json_pre_tool_exit_two_blocks_read_body() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-hook-exit-two-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("secret.txt");
        std::fs::write(&path, "must-not-read").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-read-exit-two".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": path.display().to_string()}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            tools: vec![crate::tools::file_read_tool::file_read_tool_schema()],
            ..ToolUseContext::default()
        };
        context
            .tool_permission_context
            .additional_working_directories
            .insert(
                root.display().to_string(),
                crate::types::permissions::AdditionalWorkingDirectory {
                    path: root.display().to_string(),
                    source: crate::types::permissions::PermissionRuleSource::Session,
                },
            );
        let mut queue = Vec::new();
        let request = run_tool_use(&block, &assistant, &context, &mut queue)
            .request
            .expect("read request");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{"command": "echo blocked-by-hook >&2; exit 2", "timeout": 5}]
            }]
        }))
        .unwrap();
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback = observed.clone();
        let observed_path = path.display().to_string();
        let unsubscribe = crate::tools::file_read_tool::register_file_read_listener(
            std::sync::Arc::new(move |read_path, _| {
                if read_path == observed_path {
                    callback.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(())
            }),
        );

        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant),
            &config,
            Vec::new(),
        )
        .await;
        unsubscribe();

        assert!(!observed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(result.new_context.read_file_state.is_empty());
        assert_eq!(
            result.permission_decision.choice,
            PermissionPromptChoice::Deny
        );
        // Maps to CC toolHooks.ts:481-498 / toolExecution.ts:1023-1068:
        // preserve the hook's error text; only the user-reject sentinel takes
        // the Rejected renderer branch (UserToolResultMessage.tsx:49-87).
        let block = result
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("hook denial tool result");
        let expected =
            "PreToolUse:Read hook error: [echo blocked-by-hook >&2; exit 2]: blocked-by-hook\n";
        assert!(block.is_error);
        assert_eq!(block.derived_status(), ToolResultStatus::Error);
        assert_eq!(block.content, expected);
        assert_eq!(
            block.tool_use_result,
            Some(serde_json::json!(format!("Error: {expected}")))
        );
        assert!(!context.abort_controller.is_aborted());
        assert!(!result.new_context.abort_controller.is_aborted());
        assert!(result.continuation_messages.is_empty());
        assert!(result.hook_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary {
                prevented_continuation: false,
                ..
            })
        )));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn inline_skill_expansion_processes_file_mentions_and_merges_read_state() {
        let root = std::env::temp_dir().join(format!(
            "cometix-inline-skill-attachment-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("reference.txt"), "skill reference\n").unwrap();
        let request = crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: "permission-skill-attachment".to_string(),
            tool_use_id: "toolu-skill-attachment".to_string(),
            tool_name: "Skill".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Skill".to_string(),
            message: String::new(),
            input_summary: "Skill".to_string(),
            input: serde_json::json!({"skill":"demo"}),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new("Skill", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        };
        let mut context =
            crate::tool::ToolUseContext::default().with_cwd_override(Some(root.clone()));
        let mut messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::MetaText(
                "Follow the guidance in @reference.txt".to_string(),
            )],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        append_inline_skill_attachments(&request, &mut context, &mut messages).await;
        assert!(
            messages.iter().any(|message| matches!(
                message,
                Message::Attachment(attachment)
                    if attachment.attachment_type() == "file"
                        && attachment.attachment.to_wire()["content"]["file"]["content"]
                            == "skill reference\n"
            )),
            "{messages:?}"
        );
        assert_eq!(context.read_file_state.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_read_hook_receives_raw_output_and_emits_additional_context() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-post-hook-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("source.txt");
        let capture = root.join("hook-input.json");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-read-post-hook".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": path.display().to_string()}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            tools: vec![crate::tools::file_read_tool::file_read_tool_schema()],
            ..ToolUseContext::default()
        };
        context
            .tool_permission_context
            .additional_working_directories
            .insert(
                root.display().to_string(),
                crate::types::permissions::AdditionalWorkingDirectory {
                    path: root.display().to_string(),
                    source: crate::types::permissions::PermissionRuleSource::Session,
                },
            );
        let mut queue = Vec::new();
        let request = run_tool_use(&block, &assistant, &context, &mut queue)
            .request
            .expect("read request");
        let command = format!(
            "input=$(cat); printf '%s' \"$input\" > '{}'; printf '%s' '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PostToolUse\",\"additionalContext\":\"remember this read\"}}}}'",
            capture.display()
        );
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PostToolUse": [{
                "matcher": "Read",
                "hooks": [{"command": command, "timeout": 5}]
            }]
        }))
        .unwrap();

        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant),
            &config,
            Vec::new(),
        )
        .await;

        let captured: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
        assert_eq!(captured["tool_response"]["type"], "text");
        assert_eq!(
            captured["tool_response"]["file"]["content"],
            "alpha\nbeta\n"
        );
        assert_eq!(result.post_tool_model_messages.len(), 1);
        let Message::Attachment(additional) = &result.post_tool_model_messages[0] else {
            panic!("expected hook additional-context attachment")
        };
        assert_eq!(
            additional.attachment.to_wire()["hookName"],
            "PostToolUse:Read"
        );
        assert_eq!(additional.attachment.to_wire()["hookEvent"], "PostToolUse");
        assert_eq!(
            additional.attachment.to_wire()["toolUseID"],
            "toolu-read-post-hook"
        );
        // PostToolUse delivery is terminal-only; the Read source turn was
        // already committed and remains authoritative throughout the hook.
        assert!(result.new_context.read_file_state.has(&path));
        assert!(
            result
                .new_context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains(&path.display().to_string())
        );
        let normalized = crate::utils::messages::normalize_attachment_for_api(additional, None);
        assert!(matches!(
            &normalized[0].content[0],
            crate::types::message::UserContent::MetaText(text)
                if text.contains("remember this read")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn prepare_permission_request_before_prompt_with_config_applies_hook_input_and_allow() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{\"command\":\"printf prepared\"}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let prepared = prepare_permission_request_before_prompt_with_config(
            &request,
            &context,
            Some(&config),
            vec![],
            None,
        )
        .await;

        assert_eq!(
            prepared.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert_eq!(
            prepared
                .request
                .input
                .get("command")
                .and_then(|value| value.as_str()),
            Some("printf prepared")
        );
        assert_eq!(prepared.request.input_summary, "printf prepared");
        assert_eq!(
            prepared.request.rule.rule_content.as_deref(),
            Some("printf prepared")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_prompt_read_hook_input_is_authoritative_including_allow_decision() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-pre-prompt-hook-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("original.txt");
        std::fs::write(&original, "original\n").unwrap();
        let outside = root
            .parent()
            .unwrap_or(std::path::Path::new("/tmp"))
            .join(format!(
                "cometix-read-reroute-{}.txt",
                uuid::Uuid::new_v4().simple()
            ));
        std::fs::write(&outside, "first\nsecond\n").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-read-pre-prompt".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": original.display().to_string()}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        let mut queue = Vec::new();
        let request = run_tool_use(&block, &assistant, &context, &mut queue)
            .request
            .expect("parsed Read request");

        let invalid_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{\"file_path\":5}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let invalid = prepare_permission_request_before_prompt_with_config(
            &request,
            &context,
            Some(&invalid_config),
            Vec::new(),
            None,
        )
        .await;
        assert_eq!(invalid.request.input, serde_json::json!({"file_path": 5}));

        let reroute_output = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": {
                    "file_path": outside.display().to_string(),
                    "offset": "2",
                    "limit": "1",
                    "hook_extra": true
                }
            }
        })
        .to_string();
        let reroute_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": format!("printf '%s' '{reroute_output}'"),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let rerouted = prepare_permission_request_before_prompt_with_config(
            &request,
            &context,
            Some(&reroute_config),
            Vec::new(),
            None,
        )
        .await;
        assert_eq!(
            rerouted.request.input["file_path"],
            outside.display().to_string()
        );
        assert_eq!(rerouted.request.input["hook_extra"], true);
        assert_eq!(
            rerouted.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert!(!rerouted.force_ask);
        let execution =
            streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
                &rerouted.request,
                &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
                rerouted.prevent_continuation,
                rerouted.stop_reason.as_deref(),
                &context,
                Some(&assistant),
            )
            .await;
        assert!(matches!(
            execution.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Success
                && block.content.contains("second")
                && !block.content.contains("first")
        ));

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn typed_pre_tool_hook_rewrite_reaches_final_can_use_before_edit_permission() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-hook-order-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("original.txt");
        let rewritten = root.join("rewritten.txt");
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-edit-hook-order".to_string()),
            name: "Edit".to_string(),
            input: serde_json::json!({
                "file_path": original.display().to_string(),
                "old_string": "",
                "new_string": "model"
            }),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let saw_final = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_final_callback = saw_final.clone();
        let expected = rewritten.display().to_string();
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];
        context.can_use_tool = crate::tool::CanUseToolCallback::new(
            move |_tool, input, _context, _assistant, _tool_use_id, _force| {
                saw_final_callback.store(
                    input.get("file_path").and_then(serde_json::Value::as_str)
                        == Some(expected.as_str()),
                    std::sync::atomic::Ordering::SeqCst,
                );
                crate::types::permissions::PermissionDecision::Deny {
                    message: String::new(),
                    decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                        reason: "test".to_string(),
                    },
                    tool_use_id: None,
                }
            },
        );
        let mut queue = Vec::new();
        let gate = run_tool_use(&block, &assistant, &context, &mut queue);
        assert!(!saw_final.load(std::sync::atomic::Ordering::SeqCst));
        let hook_output = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {
                    "file_path": rewritten.display().to_string(),
                    "old_string": "",
                    "new_string": "hook"
                }
            }
        })
        .to_string();
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Edit",
                "hooks": [{
                    "command": format!("printf '%s' '{hook_output}'"),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let prepared = prepare_permission_request_before_prompt_with_config(
            &gate.request.expect("parsed request"),
            &context,
            Some(&config),
            Vec::new(),
            None,
        )
        .await;
        let required = apply_required_can_use_tool_after_hooks(
            prepared.request,
            &context,
            Some(&assistant),
            prepared.forced_choice,
            prepared.hook_supplied_updated_input,
        )
        .await
        .expect("permission check should not abort");
        assert!(saw_final.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(required.forced_choice, Some(PermissionPromptChoice::Deny));
        assert_eq!(
            required
                .request
                .call_input
                .as_ref()
                .and_then(
                    |input| input.get(crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY)
                )
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            Some(rewritten.display().to_string())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn permission_request_hooks_run_only_at_the_prompt_boundary() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PermissionRequest": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PermissionRequest\",\"decision\":{\"behavior\":\"allow\",\"updatedInput\":{\"command\":\"printf permission-hook\"},\"updatedPermissions\":[{\"type\":\"addRules\",\"destination\":\"session\",\"behavior\":\"allow\",\"rules\":[{\"toolName\":\"Bash\",\"ruleContent\":\"printf permission-hook\"}]}]}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let pre_tool_only = prepare_permission_request_before_prompt_with_config(
            &request,
            &context,
            Some(&config),
            vec![],
            None,
        )
        .await;
        assert!(pre_tool_only.forced_choice.is_none());
        assert!(pre_tool_only.permission_updates.is_empty());

        let prepared = prepare_permission_prompt_hooks_with_config(
            &request,
            &context,
            Some(&config),
            vec![],
            None,
        )
        .await;

        assert_eq!(
            prepared.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert_eq!(
            prepared
                .request
                .input
                .get("command")
                .and_then(|value| value.as_str()),
            Some("printf permission-hook")
        );
        assert_eq!(prepared.permission_updates.len(), 1);
        assert!(prepared.hook_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(SystemMessage::StopHookSummary { hook_label: Some(label), .. })
                if label == "PermissionRequest"
        )));
    }

    #[tokio::test]
    async fn pre_tool_continue_false_runs_allowed_tool_then_emits_stop_attachment() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_pre_stop_after_run".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "printf tool-ran"}),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();
        let request = run_tool_use(&block, &assistant_message, &context, &mut queue)
            .request
            .expect("permission request");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"continue\":false,\"stopReason\":\"stop after tool\",\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let result = check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            &config,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Success
                && block.content == "tool-ran"
        ));
        assert!(matches!(
            result.continuation_messages.as_slice(),
            [Message::Attachment(attachment)]
                if attachment.attachment_type() == "hook_stopped_continuation"
                    && attachment.attachment.to_wire()["message"] == "stop after tool"
        ));
    }

    #[tokio::test]
    async fn prechecked_execution_does_not_rerun_pre_tool_hooks() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"should not rerun\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let result = check_permissions_and_call_tool_with_config_inner(
            &request,
            PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
            &context,
            None,
            &config,
            vec![],
            false,
            false,
            None,
        )
        .await;

        assert_eq!(
            result.permission_decision.choice,
            PermissionPromptChoice::AllowOnce
        );
        assert!(result.tool_result.is_some());
        assert!(result.hook_messages.iter().all(|message| !matches!(
            &message.kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content: text, .. })
                if text == "should not rerun"
        )));
    }

    #[tokio::test]
    async fn streamed_check_permissions_and_call_tool_with_config_applies_pre_tool_updated_input() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_pre_updated_input".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "printf before-hook" }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"command\":\"printf hook-updated\"}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let result = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            Some(&config),
            vec![],
        )
        .await;

        let tool_result = result
            .tool_result
            .expect("updated-input execution should produce tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_pre_updated_input");
                assert!(result.content.contains("hook-updated"));
                assert!(!result.content.contains("before-hook"));
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[tokio::test]
    async fn grep_pre_tool_updated_input_reparses_semantic_values_before_execution() {
        let root = std::env::temp_dir().join(format!(
            "cometix-grep-hook-semantic-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "Needle\nneedle\n").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_grep_hook_semantic".to_string()),
            name: "Grep".to_string(),
            input: serde_json::json!({
                "pattern": "before-hook-no-match",
                "output_mode": "content"
            }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::with_permission_context(ToolPermissionContext::default())
        };
        // CC filesystem.ts:667-674: changing the invocation cwd is not a grant.
        // This fixture explicitly authorizes the directory before testing hook input parsing.
        context
            .tool_permission_context
            .additional_working_directories
            .insert(
                root.display().to_string(),
                crate::types::permissions::AdditionalWorkingDirectory {
                    path: root.display().to_string(),
                    source: crate::types::permissions::PermissionRuleSource::Session,
                },
            );
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("Grep permission request");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Grep",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"pattern\":\"needle\",\"output_mode\":\"content\",\"-i\":\"true\",\"head_limit\":\"1\"}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let result = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&config),
            vec![],
        )
        .await;
        let tool_result = result.tool_result.expect("hook-updated Grep result");
        let Some(UserContent::ToolResult(result)) = tool_result.content.first() else {
            panic!("expected Grep tool result");
        };
        assert!(result.content.contains("a.txt:1:Needle"));
        assert!(!result.content.contains("a.txt:2:needle"));
        assert!(
            result
                .content
                .contains("[Showing results with pagination = limit: 1]")
        );
        assert!(matches!(
            result.tool_use_result.as_ref(),
            Some(serde_json::Value::Object(raw))
                if raw.get("appliedLimit") == Some(&serde_json::json!(1))
                    && raw.get("numLines") == Some(&serde_json::json!(1))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn read_hook_updated_input_executes_directly_and_respects_hook_permission() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-hook-revalidate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let original = workspace.join("original.txt");
        let secret = outside.join("secret.txt");
        std::fs::write(&original, "original-content\nsecond-content\n").unwrap();
        std::fs::write(&secret, "must-not-read\n").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_hook_revalidate".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "original.txt"}),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext {
            cwd_override: Some(workspace.clone()),
            ..ToolUseContext::with_permission_context(ToolPermissionContext::default())
        };

        // `validateInput` calls canonical expandPath before the observable
        // clone and before any PreToolUse/permission continuation exists.
        let nul_block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_nul".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "bad\0path"}),
        };
        let mut nul_queue = Vec::new();
        let nul_update = run_tool_use(&nul_block, &assistant_message, &context, &mut nul_queue);
        assert!(nul_update.request.is_none());
        assert!(nul_queue.is_empty());
        assert!(matches!(
            nul_update.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content.contains("Path contains null bytes")
        ));
        assert!(context.read_file_state.is_empty());

        let mut queue = Vec::new();
        let request = run_tool_use(&block, &assistant_message, &context, &mut queue)
            .request
            .expect("Read permission request");

        let invalid_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"file_path\":5}}}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let passthrough = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&invalid_config),
            vec![],
        )
        .await;
        let Some(UserContent::ToolResult(passthrough_result)) = passthrough
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected hook-updated Read result")
        };
        assert!(passthrough_result.is_error);
        assert_eq!(passthrough_result.content, "Invalid Read tool input");
        assert!(!passthrough_result.content.contains("original-content"));

        let semantic_payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {
                    "file_path": original,
                    "offset": "2",
                    "limit": "1"
                }
            }
        })
        .to_string();
        let semantic_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": format!("printf '%s' '{}'", semantic_payload),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let semantic = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&semantic_config),
            vec![],
        )
        .await;
        let Some(UserContent::ToolResult(semantic_result)) = semantic
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected semantic Read result")
        };
        let expected_numbered = "02\tsecond-content\n12\t";
        // The reminder is MODEL-gated, not audience-gated: CC
        // `FileReadTool.ts:734-738` exempts `claude-opus-4-6`. But the default
        // main loop model IS audience-gated (`model.rs:52-63` — ants default to
        // Opus with 1M context, everyone else to Sonnet), and this message
        // carries `model: None`, so the two builds resolve different models and
        // land on opposite sides of that exemption. Derive the expectation from
        // the same gate the tool uses instead of hardcoding one audience.
        let resolved = crate::utils::model::model::get_main_loop_model();
        let expected =
            if crate::utils::model::model::get_canonical_name(&resolved) == "claude-opus-4-6" {
                expected_numbered.to_string()
            } else {
                format!(
                    "{expected_numbered}{}",
                    crate::tools::file_read_tool::CYBER_RISK_MITIGATION_REMINDER
                )
            };
        assert_eq!(semantic_result.content, expected);
        let raw_semantic = semantic_result
            .tool_use_result
            .as_ref()
            .expect("raw Read output");
        assert_eq!(
            raw_semantic,
            &serde_json::json!({
                "type": "text",
                "file": {
                    "filePath": "original.txt",
                    "content": "second-content\n",
                    "numLines": 2,
                    "startLine": "2",
                    "totalLines": 3
                }
            })
        );
        assert!(crate::tools::file_read_tool::parse_output(raw_semantic).is_none());
        // The unparseable raw rides the row (rendering nothing, as CC's
        // safeParse-failure branch does); the display carries no Read shape.
        let semantic_block = semantic
            .message
            .as_ref()
            .and_then(user_tool_result_block)
            .expect("semantic read row");
        // Schema-rejected output: visibility is a render-time decision, while
        // the raw itself still rides the row.
        assert_eq!(semantic_block.tool_use_result.as_ref(), Some(raw_semantic));
        let cached = context
            .read_file_state
            .get(&original)
            .expect("Read cache state");
        assert_eq!(cached.offset, Some(serde_json::json!("2")));
        assert_eq!(cached.limit, Some(serde_json::json!("1")));

        // Cold/recovered success uses the same strict parser and stays hidden,
        // while retaining the exact raw output value for round trips.
        // The cold path yields no Read display shape; the raw rides the
        // row (session_storage fills `tool_use_result` from the JSONL key) and
        // an unparseable raw renders nothing, as CC's safeParse-failure branch.
        assert!(
            crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
                "Read",
                ToolResultStatus::Success,
                &semantic_result.content,
                Some(raw_semantic),
                None,
                &[],
                crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
            )
            .is_empty()
        );

        // Zod `z.object` accepts and strips unknown keys for rendering, while
        // the unparsed `toolUseResult` remains byte-shape-equivalent metadata.
        let valid_with_unknown_keys = serde_json::json!({
            "type": "text",
            "unknownOuter": true,
            "file": {
                "filePath": "original.txt",
                "content": "original-content",
                "numLines": 1,
                "startLine": 1,
                "totalLines": 1,
                "unknownFile": [1, 2, 3]
            }
        });
        // A parseable raw yields a visible row — the raw itself rides
        // the row and feeds the by-tool-name renderer.
        assert!(
            !crate::components::messages::user_tool_result_message::render_tool_result_lines_for_result(
                "Read",
                ToolResultStatus::Success,
                "1\toriginal-content",
                Some(&valid_with_unknown_keys),
                None,
                &[],
                crate::components::messages::user_tool_result_message::utils::ToolRenderOptions::default(),
            )
            .is_empty()
        );

        let alternate = workspace.join("alternate.txt");
        std::fs::write(&alternate, "alternate-content\n").unwrap();
        let reroute_payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {"file_path": alternate}
            }
        })
        .to_string();
        let reroute_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": format!("printf '%s' '{}'", reroute_payload),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let rerouted = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&reroute_config),
            vec![],
        )
        .await;
        let Some(UserContent::ToolResult(rerouted_result)) = rerouted
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected rerouted Read result")
        };
        assert!(rerouted_result.content.contains("alternate-content"));
        assert!(!rerouted_result.content.contains("original-content"));

        // CC validates the model input before hooks, then calls Read directly
        // with updatedInput. A semantic validation rejection such as a binary
        // extension therefore does not silently restore the original input.
        let hook_binary = workspace.join("hook-binary.exe");
        std::fs::write(&hook_binary, "hook-binary-content\n").unwrap();
        let binary_payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {"file_path": hook_binary}
            }
        })
        .to_string();
        let binary_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": format!("printf '%s' '{}'", binary_payload),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let binary = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&binary_config),
            vec![],
        )
        .await;
        let Some(UserContent::ToolResult(binary_result)) = binary
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected hook-updated binary-extension Read result")
        };
        assert!(!binary_result.is_error);
        assert!(binary_result.content.contains("hook-binary-content"));
        assert!(!binary_result.content.contains("original-content"));

        let hook_payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": {"file_path": secret}
            }
        })
        .to_string();
        let allowed_config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Read",
                "hooks": [{
                    "command": format!("printf '%s' '{}'", hook_payload),
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();
        let allowed = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            Some(&assistant_message),
            Some(&allowed_config),
            vec![],
        )
        .await;
        assert_eq!(
            allowed.permission_decision.choice,
            PermissionPromptChoice::AllowOnce
        );
        let Some(UserContent::ToolResult(allowed_result)) = allowed
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected hook-allowed Read result")
        };
        assert!(!allowed_result.is_error, "{}", allowed_result.content);
        assert!(allowed_result.content.contains("must-not-read"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let link = workspace.join("secret-link.txt");
            symlink(&secret, &link).unwrap();
            let absolute_rule = format!("//{}", link.display().to_string().trim_start_matches('/'));
            let mut denied_context = context.clone();
            denied_context
                .tool_permission_context
                .always_deny_rules
                .insert(
                    crate::types::permissions::PermissionRuleSource::Session,
                    vec![PermissionRuleValue::new("Read", Some(absolute_rule))],
                );
            let symlink_payload = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "updatedInput": {"file_path": link}
                }
            })
            .to_string();
            let symlink_config: RegisteredHooks = registered_from_value(serde_json::json!({
                "PreToolUse": [{
                    "matcher": "Read",
                    "hooks": [{
                        "command": format!("printf '%s' '{}'", symlink_payload),
                        "timeout": 5
                    }]
                }]
            }))
            .unwrap();
            let prepared = prepare_permission_request_before_prompt_with_config(
                &request,
                &denied_context,
                Some(&symlink_config),
                Vec::new(),
                None,
            )
            .await;
            let symlink_blocked = apply_required_can_use_tool_after_hooks(
                prepared.request,
                &denied_context,
                Some(&assistant_message),
                prepared.forced_choice,
                prepared.hook_supplied_updated_input,
            )
            .await
            .expect("permission check should not abort");
            assert_eq!(
                symlink_blocked.forced_choice,
                Some(PermissionPromptChoice::Deny)
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn streamed_check_permissions_and_call_tool_with_config_runs_post_tool_failure_hooks() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_read_failure_hook".to_string()),
            // CC parity note: a non-zero Bash exit is NOT an error result
            // (BashTool map sets `is_error: interrupted` only), so use a
            // genuinely erroring Read to exercise PostToolUseFailure.
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "/nonexistent/cometix-hook-test" }),
        };
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant_message, &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PostToolUseFailure": [{
                "matcher": "Read",
                "hooks": [{ "command": "echo failure hook ran", "timeout": 5 }]
            }]
        }))
        .unwrap();

        let result = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            Some(&config),
            vec![],
        )
        .await;

        assert!(result.tool_result.is_some());
        assert!(result.hook_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content: text, .. })
                if text.trim() == "failure hook ran"
        )));
    }

    #[tokio::test]
    async fn streamed_check_permissions_and_call_tool_with_config_blocks_on_pre_tool_hook() {
        let mut queue = Vec::new();
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);
        let request = update.request.expect("permission request should exist");
        let config: RegisteredHooks = registered_from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "command": "printf '%s' '{\"decision\":\"block\",\"reason\":\"blocked by pre hook\"}'",
                    "timeout": 5
                }]
            }]
        }))
        .unwrap();

        let result = streamed_check_permissions_and_call_tool_with_config(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
            Some(&config),
            vec![],
        )
        .await;

        assert_eq!(
            result.permission_decision.choice,
            PermissionPromptChoice::Deny
        );
        assert!(result.hook_messages.iter().any(|message| matches!(
            &message.kind,
            RenderableMessageKind::System(crate::types::message::SystemMessage::Informational { content: text, .. })
                if text == "blocked by pre hook"
        )));
        let tool_result = result
            .tool_result
            .expect("blocked hook still returns tool_result");
        match &tool_result.content[0] {
            UserContent::ToolResult(result) => {
                assert_eq!(result.tool_use_id.0, "toolu_mock");
                assert!(result.is_error);
            }
            other => panic!("unexpected user content: {other:?}"),
        }
    }

    #[test]
    fn in_memory_allow_rule_skips_permission_request() {
        let mut ctx = ToolPermissionContext::default();
        let mut rules = HashMap::new();
        rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        ctx.always_allow_rules = rules;
        let context = ToolUseContext::with_permission_context(ctx);
        let mut queue = Vec::new();

        let update = run_tool_use_permission_gate(&queued_bash(), &context, &mut queue);

        assert!(!update.blocked_on_permission);
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn hook_allow_still_runs_required_can_use_without_forcing_and_rewrites_input() {
        let request = mock_permission_request_with_input(
            "perm-required",
            "toolu-required",
            "Edit",
            "/original/file",
            serde_json::json!({
                "file_path":"/original/file",
                "old_string":"old",
                "new_string":"new"
            }),
            PermissionMode::Default,
        );
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let saw_force = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_force_for_callback = saw_force.clone();
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];
        context.require_can_use_tool = true;
        context.can_use_tool = crate::tool::CanUseToolCallback::new_async(
            move |_tool, input, _context, _assistant, _tool_use_id, force| {
                saw_force_for_callback.store(force.is_some(), std::sync::atomic::Ordering::SeqCst);
                let mut input = input.clone();
                Box::pin(async move {
                    input["file_path"] = serde_json::Value::String("/overlay/file".into());
                    crate::types::permissions::PermissionDecision::Allow {
                        updated_input: Some(input),
                        user_modified: None,
                        decision_reason: None,
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    }
                })
            },
        );

        let required = apply_required_can_use_tool_after_hooks(
            request,
            &context,
            Some(&assistant),
            Some(PermissionPromptChoice::AllowOnce),
            false,
        )
        .await
        .expect("permission check should not abort");

        assert!(!saw_force.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            required.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert_eq!(
            required
                .request
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str),
            Some("/overlay/file")
        );
        assert_eq!(
            required
                .request
                .call_input
                .as_ref()
                .and_then(|input| {
                    input.get(crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY)
                })
                .and_then(serde_json::Value::as_str),
            Some("/overlay/file")
        );
    }

    /// Maps to: CC `toolHooks.ts:350-356` — `interactionSatisfied`. A
    /// PreToolUse hook allow that carries `updatedInput` for an interactive
    /// tool (AskUserQuestion) IS the user interaction; CC skips the duplicate
    /// `canUseTool` call and only re-checks deny/ask rules
    /// (`checkRuleBasedPermissions`, which has no interactive re-ask step —
    /// `permissions.ts:1071-1157` returns null here), so the hook allow
    /// stands (`toolHooks.ts:378-385`).
    ///
    /// Old shape (gate was `hook_allowed && requires_user_interaction` with no
    /// `interactionSatisfied` conjunct): the required-callback leg re-invoked
    /// `canUseTool`, and a plain context with no callback resolved `None` →
    /// Deny. This is asserted below as the `hook_supplied_updated_input=false`
    /// leg, which still re-prompts (force_ask) per CC's unsatisfied branch.
    #[tokio::test]
    async fn hook_updated_input_satisfies_interaction_and_skips_duplicate_can_use_tool() {
        let input = serde_json::json!({
            "questions": [{
                "question": "Proceed?",
                "header": "Proceed",
                "options": [
                    {"label": "Yes", "description": "Continue"},
                    {"label": "No", "description": "Stop"}
                ]
            }],
            "answers": {"Proceed?": "Yes"}
        });
        let make_request = || {
            mock_permission_request_with_input(
                "perm-ask-satisfied",
                "toolu-ask-satisfied",
                "AskUserQuestion",
                "Answer questions?",
                input.clone(),
                PermissionMode::Default,
            )
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::ask_user_question_tool::ask_user_question_tool_schema()];
        // No canUseTool callback and no requireCanUseTool: the plain context
        // in which the old shape denied outright.
        assert!(!context.can_use_tool.is_some());
        assert!(!context.require_can_use_tool);

        // interactionSatisfied: hook allow + updatedInput → rule re-check only,
        // and the hook allow stands (no rule objects here).
        let satisfied = apply_required_can_use_tool_after_hooks(
            make_request(),
            &context,
            Some(&assistant),
            Some(PermissionPromptChoice::AllowOnce),
            true,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(
            satisfied.forced_choice,
            Some(PermissionPromptChoice::AllowOnce),
            "hook-satisfied interaction must not re-invoke canUseTool"
        );
        assert!(!satisfied.force_ask);

        // Unsatisfied (allow without updatedInput): CC's
        // `(requiresInteraction && !interactionSatisfied)` branch re-enters
        // canUseTool; with none present the port denies (fail-closed).
        let unsatisfied = apply_required_can_use_tool_after_hooks(
            make_request(),
            &context,
            Some(&assistant),
            Some(PermissionPromptChoice::AllowOnce),
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(
            unsatisfied.forced_choice,
            Some(PermissionPromptChoice::Deny)
        );
    }

    #[tokio::test]
    async fn normal_can_use_tool_rechecks_final_hook_input_and_re_pins_edit_destination() {
        let root = std::env::temp_dir().join(format!(
            "cometix-final-hook-permission-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let original = root.join("original.txt");
        let rewritten = root.join("rewritten.txt");
        let mut request = mock_permission_request_with_input(
            "perm-final-hook",
            "toolu-final-hook",
            "Edit",
            rewritten.display().to_string(),
            serde_json::json!({
                "file_path": rewritten.display().to_string(),
                "old_string": "old",
                "new_string": "new"
            }),
            PermissionMode::Default,
        );
        request.call_input = Some(serde_json::json!({
            "file_path": original.display().to_string(),
            "old_string": "old",
            "new_string": "new",
            crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY:
                original.display().to_string()
        }));
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let saw_rewritten = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_rewritten_callback = saw_rewritten.clone();
        let expected = rewritten.display().to_string();
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];
        context.can_use_tool = crate::tool::CanUseToolCallback::new(
            move |_tool, input, _context, _assistant, _tool_use_id, _force| {
                saw_rewritten_callback.store(
                    input.get("file_path").and_then(serde_json::Value::as_str)
                        == Some(expected.as_str()),
                    std::sync::atomic::Ordering::SeqCst,
                );
                crate::types::permissions::PermissionDecision::Deny {
                    message: String::new(),
                    decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                        reason: "test".to_string(),
                    },
                    tool_use_id: None,
                }
            },
        );
        let required = apply_required_can_use_tool_after_hooks(
            request,
            &context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");
        assert!(saw_rewritten.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(required.forced_choice, Some(PermissionPromptChoice::Deny));
        assert_eq!(
            required
                .request
                .call_input
                .as_ref()
                .and_then(
                    |input| input.get(crate::tools::file_edit_tool::CHECKED_EDIT_APPROVED_PATH_KEY)
                )
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            Some(rewritten.display().to_string())
        );
    }

    #[tokio::test]
    async fn hook_allow_rechecks_edit_deny_ask_and_safety_rules() {
        let cwd = crate::bootstrap::state::get_original_cwd();
        let ordinary = cwd.join("hook-allowed-edit.txt");
        let safety = cwd.join(".git/config");
        let make_request = |path: &std::path::Path| {
            mock_permission_request_with_input(
                "perm-hook-rule-check",
                "toolu-hook-rule-check",
                "Edit",
                path.display().to_string(),
                serde_json::json!({
                    "file_path": path.display().to_string(),
                    "old_string": "old",
                    "new_string": "new"
                }),
                PermissionMode::Default,
            )
        };
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];

        let allowed = apply_required_can_use_tool_after_hooks(
            make_request(&ordinary),
            &context,
            None,
            Some(PermissionPromptChoice::AllowOnce),
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(
            allowed.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert!(!allowed.force_ask);

        let safety_checked = apply_required_can_use_tool_after_hooks(
            make_request(&safety),
            &context,
            None,
            Some(PermissionPromptChoice::AllowOnce),
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(safety_checked.forced_choice, None);
        assert!(safety_checked.force_ask);

        let mut denied_context = context.clone();
        denied_context
            .tool_permission_context
            .always_deny_rules
            .insert(
                crate::types::permissions::PermissionRuleSource::Session,
                // Absolute-path rules are written `//path` at the source: the
                // leading pair is what `pattern_with_root` (filesystem.rs:200)
                // strips to anchor the pattern at `/`, matching the relative
                // path the matcher builds. A bare `/path` falls into the
                // :225 branch, which keeps the full absolute string as the
                // pattern and then compares it against a root-relative path —
                // it can never match.
                vec![PermissionRuleValue::new(
                    "Edit",
                    Some(format!("/{}", ordinary.display())),
                )],
            );
        let denied = apply_required_can_use_tool_after_hooks(
            make_request(&ordinary),
            &denied_context,
            None,
            Some(PermissionPromptChoice::AllowOnce),
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(denied.forced_choice, Some(PermissionPromptChoice::Deny));
        assert!(!denied.force_ask);
    }

    #[tokio::test]
    async fn async_fork_can_use_tool_updated_input_reaches_execution_request_like_official() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_overlay_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path":"/original/file"}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        context.can_use_tool = crate::tool::CanUseToolCallback::new_async(
            |_tool, input, _context, _assistant, _tool_use_id, _force| {
                let mut updated = input.clone();
                Box::pin(async move {
                    updated["file_path"] = serde_json::Value::String("/overlay/file".into());
                    crate::types::permissions::PermissionDecision::Allow {
                        updated_input: Some(updated),
                        user_modified: None,
                        decision_reason: None,
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    }
                })
            },
        );
        let mut queue = Vec::new();

        let update = run_tool_use(&block, &assistant, &context, &mut queue);

        assert_eq!(
            update
                .request
                .as_ref()
                .and_then(|request| request.input.get("file_path"))
                .and_then(serde_json::Value::as_str),
            Some("/original/file"),
            "pre-hook gate must not invoke canUseTool"
        );
        let required = apply_required_can_use_tool_after_hooks(
            update.request.expect("parsed request"),
            &context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(
            required
                .request
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str),
            Some("/overlay/file")
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn fork_hard_deny_dominates_hook_auto_allow() {
        assert_eq!(
            merge_forced_permission_choices(
                Some(PermissionPromptChoice::AllowOnce),
                Some(PermissionPromptChoice::Deny),
            ),
            Some(PermissionPromptChoice::Deny)
        );
    }

    #[tokio::test]
    async fn fork_can_use_tool_callback_denies_all_automatic_approval_modes_like_official() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_side_read".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path":"Cargo.toml"}),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        for mode in [
            PermissionMode::Default,
            PermissionMode::BypassPermissions,
            PermissionMode::DontAsk,
        ] {
            let mut permission_context = ToolPermissionContext {
                mode,
                ..Default::default()
            };
            permission_context.always_allow_rules.insert(
                crate::types::permissions::PermissionRuleSource::Session,
                vec![PermissionRuleValue::new(
                    "Read",
                    Some("Cargo.toml".to_string()),
                )],
            );
            let mut context = ToolUseContext::with_permission_context(permission_context);
            context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
            context.can_use_tool = crate::tool::CanUseToolCallback::new(
                |_tool, _input, _context, _assistant, _tool_use_id, _force| {
                    crate::types::permissions::PermissionDecision::Deny {
                        message: "Side questions cannot use tools".to_string(),
                        decision_reason:
                            crate::types::permissions::PermissionDecisionReason::Other {
                                reason: "side_question".to_string(),
                            },
                        tool_use_id: None,
                    }
                },
            );
            let mut queue = Vec::new();

            let update = run_tool_use(&block, &assistant, &context, &mut queue);

            assert!(update.blocked_on_permission, "mode={mode:?}");
            assert_eq!(update.forced_choice, None, "mode={mode:?}");
            let required = apply_required_can_use_tool_after_hooks(
                update.request.expect("parsed request"),
                &context,
                Some(&assistant),
                None,
                false,
            )
            .await
            .expect("permission check should not abort");
            assert_eq!(
                required.forced_choice,
                Some(PermissionPromptChoice::Deny),
                "mode={mode:?}"
            );
            assert!(queue.is_empty(), "mode={mode:?}");
        }
    }

    /// #156: a required-callback Deny delivers its `message` and
    /// `decisionReason` into the returned request. Downstream, that `message`
    /// is what `PermissionPromptResponse::with_decision_message` +
    /// `permission_terminal_result_for_decision` hand the model (CC
    /// `toolExecution.ts:1023` `let errorMessage = permissionDecision.message`),
    /// and `decision_reason` is what `PermissionRuleExplanation` renders. The
    /// pre-#156 `PromptDecision` pipe could not carry either — the request came
    /// back with the mock's empty `message` and `decision_reason: None`.
    #[tokio::test]
    async fn required_callback_deny_message_and_reason_reach_the_request() {
        let request = mock_permission_request_with_input(
            "perm-deny-detail",
            "toolu-deny-detail",
            "Read",
            "/original/file",
            serde_json::json!({"file_path":"/original/file"}),
            PermissionMode::Default,
        );
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        context.require_can_use_tool = true;
        context.can_use_tool = crate::tool::CanUseToolCallback::new(
            |_tool, _input, _context, _assistant, _tool_use_id, _force| {
                crate::types::permissions::PermissionDecision::Deny {
                    message: "Tool use is not allowed during compaction".to_string(),
                    decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                        reason: "compaction agent should only produce text summary".to_string(),
                    },
                    tool_use_id: None,
                }
            },
        );

        let required = apply_required_can_use_tool_after_hooks(
            request,
            &context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");

        assert_eq!(required.forced_choice, Some(PermissionPromptChoice::Deny));
        assert_eq!(
            required.request.message,
            "Tool use is not allowed during compaction"
        );
        assert_eq!(
            required.request.decision_reason,
            Some(crate::types::permissions::PermissionDecisionReason::Other {
                reason: "compaction agent should only produce text summary".to_string(),
            })
        );

        // The rail the message rides to the model: a system deny resolves with
        // `decision_message = request.message`, which becomes the tool_result
        // error text verbatim.
        let terminal = permission_terminal_result_for_decision(
            &required.request,
            ToolResultStatus::Rejected,
            None,
            Some(required.request.message.as_str()),
            false,
        );
        let block = user_tool_result_block(&terminal).expect("rejected tool_result block");
        assert_eq!(block.content, "Tool use is not allowed during compaction");
    }

    /// The allow that leaves this layer is CC's `buildAllow` PROJECTION, not the
    /// raw decision.
    ///
    /// Maps to: CC `hooks/useCanUseTool.tsx:113-134` — whatever the gate or a
    /// `forceDecision` produced, an allow is rebuilt before it crosses the
    /// `canUseTool` boundary:
    ///   `resolve(ctx.buildAllow(result.updatedInput ?? input,
    ///            { decisionReason: result.decisionReason }))`
    /// and `buildAllow` (`hooks/toolPermission/PermissionContext.ts:264-284`)
    /// emits only `{ behavior:'allow', updatedInput, userModified:false,
    /// decisionReason? }`. `toolUseID` / `acceptFeedback` / `contentBlocks` are
    /// dropped by CC itself here, so the `..` in
    /// `CanUseToolResult::Allow { updated_input, .. }` is the CC shape rather
    /// than a lossy flatten.
    ///
    /// #185: this is the CONSUMER half of the pin. The callback below still
    /// hands in a maximal six-field allow, but the rebuild now happens up in
    /// `hooks/use_can_use_tool.rs#forced_decision_to_can_use_tool_result`, so
    /// the three extras are already gone by the time this arm destructures.
    /// The producer half is
    /// `use_can_use_tool.rs#forced_allow_decision_projects_official_build_allow_shape`.
    ///
    /// The half after the gate is what makes the pin load-bearing: the no-dialog
    /// response the query actor builds for an allow carries no feedback and no
    /// content blocks, so the append chain adds nothing — matching CC, where
    /// `toolExecution.ts:1421-1438` finds neither field on a rebuilt allow.
    /// Forwarding the callback allow's `accept_feedback`/`content_blocks` into
    /// that chain instead would append text CC never appends here, and would
    /// double it for a dialog allow (which already rides
    /// `PermissionPromptResponse.feedback`).
    #[tokio::test]
    async fn required_can_use_tool_allow_projects_official_build_allow_shape() {
        let request = mock_permission_request_with_input(
            "perm-allow-projection",
            "toolu-allow-projection",
            "Read",
            "/original/file",
            serde_json::json!({"file_path":"/original/file"}),
            PermissionMode::Default,
        );
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        // A `CanUseToolFn` allow carrying every `PermissionAllowDecision` field
        // (`types/permissions.ts:174-184`).
        context.can_use_tool = crate::tool::CanUseToolCallback::new(
            |_tool, input, _context, _assistant, _tool_use_id, _force| {
                let mut updated = input.clone();
                updated["file_path"] = serde_json::Value::String("/overlay/file".to_string());
                crate::types::permissions::PermissionDecision::Allow {
                    updated_input: Some(updated),
                    user_modified: Some(true),
                    decision_reason: Some(
                        crate::types::permissions::PermissionDecisionReason::Other {
                            reason: "callback allow".to_string(),
                        },
                    ),
                    tool_use_id: Some("toolu-allow-projection".to_string()),
                    accept_feedback: Some("looks good".to_string()),
                    content_blocks: vec![serde_json::json!({"type":"text","text":"extra"})],
                }
            },
        );

        let required = apply_required_can_use_tool_after_hooks(
            request,
            &context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");

        // `buildAllow(result.updatedInput ?? input, …)` — the one allow field
        // this layer consumes.
        assert_eq!(
            required
                .request
                .input
                .get("file_path")
                .and_then(serde_json::Value::as_str),
            Some("/overlay/file")
        );
        assert_eq!(
            required.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert!(!required.force_ask);
        // CC's allow variant has no `message` (`types/permissions.ts:174-184`),
        // so nothing off the decision may write one onto the request.
        assert!(required.request.message.is_empty());
        assert!(required.permission_updates.is_empty());

        // The response `query.rs` builds when no dialog ran, and the append
        // chain CC's `addToolResult` maps to.
        let response = PermissionPromptResponse::new(required.forced_choice.expect("allow choice"))
            .with_decision_message(required.request.message.clone());
        assert!(
            response.feedback.is_none(),
            "a config/callback allow has no acceptFeedback in CC"
        );
        assert!(
            response.content_blocks.is_empty(),
            "a config/callback allow has no contentBlocks in CC"
        );

        let executed = RenderableMessage::user_tool_result(
            "allow-projection-result",
            "toolu-allow-projection",
            "ok",
            false,
        );
        let mut model = append_accept_feedback_to_tool_result(
            transcript_tool_result_to_model_message(&executed, "Read"),
            &response,
            response.choice,
        );
        append_permission_content_blocks(&mut model, &response);
        let model = model.expect("model tool_result should exist");
        assert_eq!(
            model.content.len(),
            1,
            "CC appends nothing after the tool_result for a rebuilt allow"
        );
        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result)) if result.content == "ok"
        ));
    }

    fn hook_reason(hook_name: &str) -> crate::types::permissions::PermissionDecisionReason {
        crate::types::permissions::PermissionDecisionReason::Hook {
            hook_name: hook_name.to_string(),
            hook_source: None,
            reason: None,
        }
    }

    /// Maps to: CC `services/tools/toolExecution.ts:979-993`.
    ///
    /// The trigger is all three conjuncts — `decisionReason.type === 'hook'`,
    /// `hookName === 'PermissionRequest'`, `behavior !== 'ask'` — and the
    /// payload is exactly `{type, decision, toolUseID, hookEvent}` with
    /// `hookEvent` as CC's literal.
    ///
    /// Old shape: no producer existed at all, so every assertion below failed
    /// with "no such function"; the variant at `utils/attachments.rs:467` and
    /// the renderer at `components/messages/attachment_message.rs:291` were
    /// consumption-only.
    #[test]
    fn hook_permission_decision_attachment_mirrors_cc_trigger_and_payload() {
        use crate::types::permissions::PermissionBehavior;

        let base = mock_permission_request_with_input(
            "perm-hook-attachment",
            "toolu-hook-attachment",
            "Read",
            "/tmp/file",
            serde_json::json!({"file_path":"/tmp/file"}),
            PermissionMode::Default,
        );
        let no_response = PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce);

        // `decision: permissionDecision.behavior` — the union CC writes is
        // `'allow' | 'deny'` (`utils/attachments.ts:378-383`).
        let mut hooked = base.clone();
        hooked.decision_reason = Some(hook_reason("PermissionRequest"));
        let allowed =
            hook_permission_decision_attachment(&hooked, &no_response, PermissionBehavior::Allow)
                .expect("hook allow emits the attachment");
        assert_eq!(
            allowed.attachment,
            crate::types::message::Attachment::HookPermissionDecision {
                decision: "allow".to_string(),
                tool_use_id: "toolu-hook-attachment".to_string(),
                hook_event: "PermissionRequest".to_string(),
            }
        );
        let denied =
            hook_permission_decision_attachment(&hooked, &no_response, PermissionBehavior::Deny)
                .expect("hook deny emits the attachment");
        assert_eq!(
            denied.attachment,
            crate::types::message::Attachment::HookPermissionDecision {
                decision: "deny".to_string(),
                tool_use_id: "toolu-hook-attachment".to_string(),
                hook_event: "PermissionRequest".to_string(),
            }
        );
        // CC `behavior !== 'ask'`.
        assert!(
            hook_permission_decision_attachment(&hooked, &no_response, PermissionBehavior::Ask)
                .is_none()
        );

        // A PreToolUse hook decision carries `hookName: 'PreToolUse:<tool>'`
        // (`toolHooks.ts:492-494,512-518`) and fails CC's `hookName` test.
        let mut pre_tool = base.clone();
        pre_tool.decision_reason = Some(hook_reason("PreToolUse:Read"));
        assert!(
            hook_permission_decision_attachment(&pre_tool, &no_response, PermissionBehavior::Deny)
                .is_none()
        );

        // Non-hook reasons and no reason at all: CC's `decisionReason?.type ===
        // 'hook'` conjunct.
        let mut ruled = base.clone();
        ruled.decision_reason = Some(crate::types::permissions::PermissionDecisionReason::Other {
            reason: "rule".to_string(),
        });
        assert!(
            hook_permission_decision_attachment(&ruled, &no_response, PermissionBehavior::Allow)
                .is_none()
        );
        assert!(
            hook_permission_decision_attachment(&base, &no_response, PermissionBehavior::Allow)
                .is_none()
        );

        // CC has ONE decision object; this port splits it across the request
        // (system decision) and the response (the decision that resolved
        // `canUseTool`, #170). The resolving decision wins — this is the SDK
        // shape `cli/print.rs:1495-1518` produces.
        let sdk_response = PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
            .with_decision_reason(Some(hook_reason("PermissionRequest")));
        assert!(
            hook_permission_decision_attachment(&ruled, &sdk_response, PermissionBehavior::Allow)
                .is_some()
        );
        let user_response = PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce)
            .with_decision_reason(Some(
                crate::types::permissions::PermissionDecisionReason::Other {
                    reason: "dialog".to_string(),
                },
            ));
        assert!(
            hook_permission_decision_attachment(&hooked, &user_response, PermissionBehavior::Allow)
                .is_none(),
            "the decision that resolved canUseTool replaces the request's reason"
        );
    }

    /// Maps to: CC `toolExecution.ts:985` `resultingMessages.push(...)` — the
    /// row is pushed ahead of the deny result (`:1064`) and the allow result
    /// (`:1456`), so it lands in the "before the primary tool_result" slice.
    ///
    /// The attachment is a dual carrier here (CC's single message is both
    /// rendered and history): the renderable row and the model/history message
    /// share one uuid, exactly like `attachments.rs#attachment_continuation`.
    /// It normalizes to `[]` for the API (`utils/messages.ts:4260`,
    /// `messages.rs::normalize_attachment_for_api`'s catch-all), so nothing reaches the model.
    ///
    /// Old shape: `pre_tool_messages` / `pre_tool_model_messages` were both
    /// empty for a hook-decided permission — the defect.
    #[tokio::test]
    async fn permission_request_hook_decision_emits_the_attachment_before_the_tool_result() {
        let mut request = mock_permission_request_with_input(
            "perm-hook-deny-row",
            "toolu-hook-deny-row",
            "Read",
            "/tmp/hook-denied",
            serde_json::json!({"file_path":"/tmp/hook-denied"}),
            PermissionMode::Default,
        );
        request.decision_reason = Some(hook_reason("PermissionRequest"));
        request.message = "Permission denied by hook".to_string();
        let context = ToolUseContext::default();

        let result = check_permissions_and_call_tool_with_response_async(
            &request,
            &PermissionPromptResponse::new(PermissionPromptChoice::Deny),
            false,
            None,
            &context,
            None,
        )
        .await;

        let expected = crate::types::message::Attachment::HookPermissionDecision {
            decision: "deny".to_string(),
            tool_use_id: "toolu-hook-deny-row".to_string(),
            hook_event: "PermissionRequest".to_string(),
        };
        assert_eq!(result.pre_tool_messages.len(), 1);
        assert_eq!(
            result.pre_tool_messages[0].kind,
            RenderableMessageKind::Attachment(expected.clone())
        );
        assert_eq!(result.pre_tool_model_messages.len(), 1);
        let Message::Attachment(model) = &result.pre_tool_model_messages[0] else {
            panic!("the history half is an attachment message");
        };
        assert_eq!(model.attachment, expected);
        assert_eq!(
            model.uuid, result.pre_tool_messages[0].uuid,
            "CC pushes ONE message; both halves keep its identity"
        );
        // CC `messages.ts:4260` normalizes this attachment to `[]`.
        assert!(
            crate::utils::messages::normalize_attachment_for_api(model, None).is_empty(),
            "the row is transcript/UI only and never reaches the model"
        );
        // The hook-execution aggregate is untouched: no hook ran here.
        assert!(result.hook_messages.is_empty());
        assert!(result.post_tool_messages.is_empty());

        // A non-hook decision on the identical flow emits nothing.
        let mut plain = request.clone();
        plain.decision_reason = None;
        let plain_result = check_permissions_and_call_tool_with_response_async(
            &plain,
            &PermissionPromptResponse::new(PermissionPromptChoice::Deny),
            false,
            None,
            &context,
            None,
        )
        .await;
        assert!(plain_result.pre_tool_messages.is_empty());
        assert!(plain_result.pre_tool_model_messages.is_empty());
    }

    /// Maps to: CC `hooks/toolPermission/PermissionContext.ts:264-284`
    /// `buildAllow` keeping `decisionReason`, which is what
    /// `toolExecution.ts:981` reads off an ALLOW.
    ///
    /// This is the leg a headless agent's PermissionRequest-hook allow arrives
    /// on: `utils/permissions/permissions.rs:1320-1332` (CC
    /// `permissions.ts:435-442`) returns `HasPermissionsToUseToolResult::Allow`
    /// with the hook reason, and the required-callback leg above it carries the
    /// same reason off `PermissionDecision::Allow`.
    ///
    /// Old shape: both allow arms matched `{ updated_input, .. }`, so the reason
    /// was dropped and the attachment could only ever fire for a hook DENY.
    #[tokio::test]
    async fn permission_request_hook_allow_reason_reaches_the_request() {
        let make_request = || {
            mock_permission_request_with_input(
                "perm-hook-allow-reason",
                "toolu-hook-allow-reason",
                "Read",
                "/tmp/hook-allowed",
                serde_json::json!({"file_path":"/tmp/hook-allowed"}),
                PermissionMode::Default,
            )
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let allow_with_hook_reason = || {
            crate::tool::CanUseToolCallback::new(
                |_tool, _input, _context, _assistant, _tool_use_id, _force| {
                    crate::types::permissions::PermissionDecision::Allow {
                        updated_input: None,
                        user_modified: None,
                        decision_reason: Some(hook_reason("PermissionRequest")),
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    }
                },
            )
        };

        // The ordinary gate leg: the callback allow rides `effective_force` and
        // `forced_decision_to_can_use_tool_result` re-emits it with its reason.
        let mut context = ToolUseContext::default();
        context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        context.can_use_tool = allow_with_hook_reason();
        let gated = apply_required_can_use_tool_after_hooks(
            make_request(),
            &context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(gated.forced_choice, Some(PermissionPromptChoice::AllowOnce));
        assert_eq!(
            gated.request.decision_reason,
            Some(hook_reason("PermissionRequest"))
        );

        // The required-callback leg (`requireCanUseTool`), CC
        // `toolHooks.ts:356-370`.
        let mut required_context = ToolUseContext::default();
        required_context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
        required_context.require_can_use_tool = true;
        required_context.can_use_tool = allow_with_hook_reason();
        let required = apply_required_can_use_tool_after_hooks(
            make_request(),
            &required_context,
            Some(&assistant),
            None,
            false,
        )
        .await
        .expect("permission check should not abort");
        assert_eq!(
            required.request.decision_reason,
            Some(hook_reason("PermissionRequest"))
        );

        // End to end: the allow reaches execution and emits CC's `decision:
        // 'allow'` row.
        let attachment = hook_permission_decision_attachment(
            &required.request,
            &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
            crate::types::permissions::PermissionBehavior::Allow,
        )
        .expect("the hook allow emits the attachment");
        assert_eq!(
            attachment.attachment,
            crate::types::message::Attachment::HookPermissionDecision {
                decision: "allow".to_string(),
                tool_use_id: "toolu-hook-allow-reason".to_string(),
                hook_event: "PermissionRequest".to_string(),
            }
        );
    }
}

// ─── Tool executor dispatch ──────────────────────────────────────────────
// Maps to: CC `services/tools/toolExecution.ts` `checkPermissionsAndCallTool`
// invoking `tool.call(...)` (:1207), dispatched polymorphically through the
// `crate::tool::ToolCall` trait (CC `Tool` interface) over the registry
// below. Registered safe-stub tools are temporary non-parity stand-ins; an
// unregistered tool is an official no-such-tool error.

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalToolExecutor;

#[derive(Clone, Debug, Default)]
pub struct LocalToolExecutionMessageResult {
    pub message: Option<RenderableMessage>,
    /// Maps to CC `ToolResult.newMessages` yielded by `runToolUse(...)` after
    /// the primary tool_result.
    pub new_messages: Vec<Message>,
    /// Physical cwd reported by foreground shell providers.
    pub cwd_after_update: Option<std::path::PathBuf>,
    /// Validated headless/SDK StructuredOutput payload.
    pub structured_output_update: Option<serde_json::Value>,
    /// Canonical `ToolResult.data` JSON passed to PostToolUse hooks before UI
    /// projection/truncation.
    pub hook_tool_response: Option<serde_json::Value>,
    /// Multimodal / structured `tool_result.content` array items (e.g. Read
    /// image blocks). Attached onto the model `UserMessage` tool_result.
    pub model_content_blocks: Vec<crate::types::message::ToolResultContentBlock>,
    /// Maps to CC `toolExecution.ts:1400` `const toolContextModifier =
    /// result.contextModifier` — read off the tool's result right after
    /// `tool.call(...)` returns and before PostToolUse hooks run.
    pub context_modifier: Option<ToolContextModifierOperation>,
}

#[cfg(test)]
struct ParentMessageProbeTool;

#[cfg(test)]
impl crate::tool::ToolCall for ParentMessageProbeTool {
    fn name(&self) -> &'static str {
        "ParentMessageProbe"
    }

    // Test-only probe; the trait member is required (CC Tool.ts:518).
    fn prompt(
        &self,
        tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        tool.description.clone()
    }

    fn call<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _request: &'a PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        parent_message: Option<&'a AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::Composed {
                    content: format!(
                        "parent={},can_use_tool={}",
                        parent_message.is_some(),
                        can_use_tool.is_some()
                    ),
                    status: ToolResultStatus::Success,
                },
                new_messages: Vec::new(),
            }
        })
    }
}

/// Registered tool behaviors, looked up by name/alias at dispatch time.
/// Maps to: CC `tools.ts` tool list + `types/tools.ts` `findToolByName` —
/// adding a tool means registering it here, mirroring CC's TOOLS array.
static TOOL_CALL_REGISTRY: &[&dyn crate::tool::ToolCall] = &[
    #[cfg(test)]
    &ParentMessageProbeTool,
    &crate::tools::bash_tool::BashTool,
    &crate::tools::powershell_tool::PowerShellTool,
    &crate::tools::file_read_tool::FileReadTool,
    &crate::tools::file_write_tool::FileWriteTool,
    &crate::tools::file_edit_tool::FileEditTool,
    &crate::tools::glob_tool::GlobTool,
    &crate::tools::grep_tool::GrepTool,
    &crate::tools::task_output_tool::TaskOutputTool,
    &crate::tools::task_stop_tool::TaskStopTool,
    &crate::tools::task_create_tool::TaskCreateTool,
    &crate::tools::task_get_tool::TaskGetTool,
    &crate::tools::task_update_tool::TaskUpdateTool,
    &crate::tools::task_list_tool::TaskListTool,
    &crate::tools::team_create_tool::TeamCreateTool,
    &crate::tools::team_delete_tool::TeamDeleteTool,
    &crate::tools::schedule_cron_tool::CronCreateTool,
    &crate::tools::schedule_cron_tool::CronDeleteTool,
    &crate::tools::schedule_cron_tool::CronListTool,
    &crate::tools::remote_trigger_tool::RemoteTriggerTool,
    &crate::tools::tool_search_tool::ToolSearchTool,
    &crate::tools::web_fetch_tool::WebFetchTool,
    &crate::tools::web_search_tool::WebSearchTool,
    &crate::tools::lsp_tool::LspTool,
    &crate::tools::config_tool::ConfigTool,
    &crate::tools::todo_write_tool::TodoWriteTool,
    &crate::tools::ask_user_question_tool::AskUserQuestionTool,
    &crate::tools::brief_tool::BriefTool,
    &crate::tools::synthetic_output_tool::SyntheticOutputTool,
    &crate::tools::send_message_tool::SendMessageTool,
    &crate::tools::list_mcp_resources_tool::ListMcpResourcesTool,
    &crate::tools::read_mcp_resource_tool::ReadMcpResourceTool,
    &crate::tools::mcp_tool::McpTool,
    &crate::tools::enter_plan_mode_tool::EnterPlanModeTool,
    &crate::tools::exit_plan_mode_tool::ExitPlanModeTool,
    &crate::tools::enter_worktree_tool::EnterWorktreeTool,
    &crate::tools::exit_worktree_tool::ExitWorktreeTool,
    &crate::tools::skill_tool::SkillTool,
    &crate::tools::notebook_edit_tool::NotebookEditTool,
    &crate::tools::agent_tool::AgentTool,
];

/// Resolve a registered tool behavior by primary name or alias.
/// Maps to: CC `Tool.ts:358` `findToolByName` lookup semantics.
pub(crate) fn find_tool_call(name: &str) -> Option<&'static dyn crate::tool::ToolCall> {
    TOOL_CALL_REGISTRY
        .iter()
        .find(|tool| tool.name() == name || tool.aliases().contains(&name))
        .copied()
}

impl LocalToolExecutor {
    /// Maps to: CC `checkPermissionsAndCallTool` result assembly — awaits the
    /// tool's async `call` (CC Tool.ts:379) then runs the map/render phases.
    /// `assistant_message` maps to CC `checkPermissionsAndCallTool(...)`'s
    /// parent `AssistantMessage` argument; `on_progress` maps to CC
    /// `runToolUse`'s `onToolProgress` forwarding closure
    /// (toolExecution.ts:1216-1221).
    pub async fn result_after_permission(
        &self,
        request: &PermissionRequest,
        choice: PermissionPromptChoice,
        context: &crate::tool::ToolUseContext,
        assistant_message: Option<&AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    ) -> Option<RenderableMessage> {
        self.result_after_permission_with_new_messages(
            request,
            choice,
            context,
            assistant_message,
            on_progress,
        )
        .await
        .message
    }

    /// Same as `result_after_permission`, but also preserves CC
    /// `ToolResult.newMessages` for the orchestration layer.
    pub async fn result_after_permission_with_new_messages(
        &self,
        request: &PermissionRequest,
        choice: PermissionPromptChoice,
        context: &crate::tool::ToolUseContext,
        assistant_message: Option<&AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    ) -> LocalToolExecutionMessageResult {
        // Entered with a decision the permission SYSTEM already made, so CC's
        // `permissionDecision.message` (`toolExecution.ts:1023`) is the
        // request's own decision message rather than a dialog answer.
        self.check_permissions_and_call_tool(
            request,
            &PermissionPromptResponse::new(choice).with_decision_message(request.message.clone()),
            context,
            assistant_message,
            on_progress,
        )
        .await
    }

    /// Maps to: CC `services/tools/toolExecution.ts#checkPermissionsAndCallTool`.
    /// Rust receives the already-resolved retained permission response as an
    /// explicit parameter rather than closing over the React continuation.
    pub async fn check_permissions_and_call_tool(
        &self,
        request: &PermissionRequest,
        response: &PermissionPromptResponse,
        context: &crate::tool::ToolUseContext,
        assistant_message: Option<&AssistantMessage>,
        on_progress: Option<crate::tool::ToolCallProgressFn<'_>>,
    ) -> LocalToolExecutionMessageResult {
        let choice = response.choice;
        match choice {
            PermissionPromptChoice::AllowOnce | PermissionPromptChoice::AlwaysAllow => {
                match find_tool_call(&request.tool_name) {
                    Some(tool) => {
                        // Maps to: CC `checkPermissionsAndCallTool` —
                        // processedInput/callInput convergence (:759-790,
                        // :1176-1208) → `tool.call(...)` (:1209) → result map.
                        let args = 'call_input: {
                            let restore_original_path =
                                |input: &mut serde_json::Value,
                                 original: &serde_json::Value,
                                 path_key: &str| {
                                    let Some(object) = input.as_object_mut() else {
                                        return;
                                    };
                                    if let Some(original_path) = original.get(path_key) {
                                        object.insert(path_key.to_string(), original_path.clone());
                                    } else {
                                        object.remove(path_key);
                                    }
                                };
                            let parsed_request_input = parse_request_input(request);
                            let mut input = if tool.name() == "Read" {
                                parsed_request_input
                            } else {
                                tool.normalize_input(&parsed_request_input)
                            };
                            if let Some(initial_input) = request.call_input.as_ref() {
                                let observable_input =
                                    tool.backfill_observable_input(initial_input, context);
                                if tool.name() == "Read" {
                                    // CC compares only the path field at final
                                    // convergence. Preserve every other raw
                                    // hook/permission replacement unchanged.
                                    if input.get("file_path").is_some()
                                        && input.get("file_path")
                                            == observable_input.get("file_path")
                                    {
                                        restore_original_path(
                                            &mut input,
                                            initial_input,
                                            "file_path",
                                        );
                                    }
                                    break 'call_input input;
                                }
                                if input != observable_input {
                                    let path_key = match tool.name() {
                                        "Glob" | "Grep" => Some("path"),
                                        _ => None,
                                    };
                                    if let Some(path_key) = path_key {
                                        let path_rerouted =
                                            input.get(path_key) != observable_input.get(path_key);
                                        let mut observable_updated =
                                            tool.backfill_observable_input(&input, context);
                                        if !matches!(
                                            tool.validate_input(&observable_updated, context),
                                            crate::tool::ValidationResult::Ok
                                        ) {
                                            break 'call_input tool.normalize_input(initial_input);
                                        }
                                        if !path_rerouted {
                                            restore_original_path(
                                                &mut observable_updated,
                                                initial_input,
                                                path_key,
                                            );
                                        }
                                        break 'call_input observable_updated;
                                    }
                                }
                                if input.get("file_path").is_some()
                                    && input.get("file_path") == observable_input.get("file_path")
                                {
                                    restore_original_path(&mut input, initial_input, "file_path");
                                }
                            }
                            input
                        };
                        let has_compat_search_reroute = if tool.name() == "Read" {
                            false
                        } else if let Some(path_key) = match tool.name() {
                            "Glob" | "Grep" => Some("path"),
                            _ => None,
                        } {
                            request.call_input.as_ref().is_some_and(|original| {
                                let input = tool.normalize_input(&parse_request_input(request));
                                let observable = tool.backfill_observable_input(original, context);
                                input.get(path_key) != observable.get(path_key)
                            })
                        } else {
                            false
                        };
                        if has_compat_search_reroute
                            && !matches!(
                                tool.check_permissions(&args, context),
                                crate::utils::permissions::permission_result::PermissionResult::Allow { .. }
                                    | crate::utils::permissions::permission_result::PermissionResult::Passthrough { .. }
                            )
                        {
                            return LocalToolExecutionMessageResult {
                                message: Some(permission_terminal_result(
                                    request,
                                    crate::types::message::ToolResultStatus::Rejected,
                                    context.agent_id.is_some(),
                                )),
                                ..Default::default()
                            };
                        }
                        // Maps to CC `checkPermissionsAndCallTool(...)`
                        // passing the official `canUseTool` callback into
                        // `tool.call(...)`. This synchronous Rust adapter
                        // reuses the same permission engine; if a nested tool
                        // would need interactive approval, it returns an
                        // ask-shaped safe denial until nested permission
                        // continuations are fully ported.
                        let can_use_tool = |tool: &crate::types::tools::Tool,
                                            input: &serde_json::Value,
                                            tool_context: &crate::tool::ToolUseContext,
                                            parent_assistant_message: &AssistantMessage,
                                            tool_use_id: &str,
                                            force_decision: Option<
                            crate::types::permissions::PermissionDecision,
                        >| {
                            if let Some(decision) = tool_context.can_use_tool.decide(
                                tool,
                                input,
                                tool_context,
                                parent_assistant_message,
                                tool_use_id,
                                force_decision.clone(),
                            )? {
                                return Ok(decision);
                            }
                            if let Some(decision) = force_decision {
                                return Ok(decision);
                            }
                            let input_summary = permission_input_summary(&tool.name, input);
                            let nested_tool_use = ToolUsePermissionRequest {
                                tool_use_id: tool_use_id.to_string(),
                                tool_name: tool.name.clone(),
                                input_summary,
                                input: input.clone(),
                            };
                            crate::hooks::use_can_use_tool::can_use_tool(
                                CanUseToolParams::from_tool_use_context(
                                    tool_context,
                                    Some(tool),
                                    &nested_tool_use,
                                    tool_use_id,
                                    Some(parent_assistant_message),
                                    None,
                                ),
                            )
                            .into_decision()
                        };
                        // Explicit callback wins; otherwise fall back to the
                        // context-level sink threaded from the query actor.
                        let progress: Option<crate::tool::ToolCallProgressFn<'_>> = on_progress
                            .or_else(|| {
                                context
                                    .tool_progress_sink
                                    .0
                                    .as_ref()
                                    .map(|sink| sink.as_ref() as _)
                            });
                        let result = tool
                            .call(
                                &args,
                                request,
                                context,
                                Some(&can_use_tool),
                                assistant_message,
                                progress,
                            )
                            .await;
                        // Read is projected exactly once here. Its call-owned state,
                        // trigger, listener, and message effects are already committed.
                        let mut new_messages = result.new_messages.clone();
                        let cwd_after_update = match &result.data {
                            crate::tool::ToolOutput::Bash(output) => output.cwd_after.clone(),
                            _ => None,
                        };
                        let structured_output_update = match &result.data {
                            crate::tool::ToolOutput::SyntheticOutput(output) => {
                                Some(output.structured_output.clone())
                            }
                            _ => None,
                        };
                        // Maps to CC `toolExecution.ts:1400`
                        // `const toolContextModifier = result.contextModifier`.
                        // CC reads a sibling field of `data`; this port reads it
                        // off the one tool that produces one (see
                        // `tools/skill_tool/mod.rs#Output::Inline`).
                        let context_modifier = match &result.data {
                            crate::tool::ToolOutput::Skill(
                                crate::tools::skill_tool::Output::Inline {
                                    context_modifier, ..
                                },
                            ) => context_modifier
                                .clone()
                                .map(ToolContextModifierOperation::Skill),
                            _ => None,
                        };

                        let (
                            content,
                            status,
                            hook_tool_response,
                            model_content_blocks,
                            raw_override,
                        ) = match &result.data {
                            crate::tool::ToolOutput::FileReadCall(
                                crate::tools::file_read_tool::FileReadRegistryOutcome::Success(
                                    read,
                                ),
                            ) => {
                                match crate::tools::file_read_tool::FileReadTool::map_tool_result_to_tool_result_block_param(
                                        &read.data,
                                        read.memory_file_mtime_ms(),
                                        None,
                                    ) {
                                        Ok(mapped) => {
                                            // CC exposes ToolResult.newMessages only after
                                            // mapping and PostToolUse succeed. Collect them
                                            // only on this success arm so mapper failure
                                            // cannot leak call-owned supplements.
                                            new_messages.extend(read.new_messages.clone());
                                            (
                                                mapped.content,
                                                ToolResultStatus::Success,
                                                // Read renders from the raw
                                                // the trait projects;
                                                // visibility is a render-time
                                                // decision (schema-rejected
                                                // output renders nothing, as
                                                // CC's success leaf).
                                                Some(read.raw_output()),
                                                mapped.content_blocks,
                                                None,
                                            )
                                        },
                                        Err(error) => (
                                            error.message(),
                                            ToolResultStatus::Error,
                                            None,
                                            Vec::new(),
                                            // The output still holds the successful
                                            // read; the row's raw must be the error.
                                            Some(serde_json::Value::String(format!(
                                                "Error: {}",
                                                error.message()
                                            ))),
                                        ),
                                    }
                            }
                            crate::tool::ToolOutput::FileReadCall(
                                crate::tools::file_read_tool::FileReadRegistryOutcome::Failure(
                                    error,
                                ),
                            ) => (
                                error.message(),
                                ToolResultStatus::Error,
                                None,
                                Vec::new(),
                                None,
                            ),
                            _ => {
                                let (content, status) = tool
                                    .map_tool_result_to_tool_result_block_param(
                                        &result.data,
                                        &request.tool_use_id,
                                    );
                                (
                                    content,
                                    status,
                                    None,
                                    model_content_blocks_from_tool_output(&result.data),
                                    None,
                                )
                            }
                        };
                        // Maps to CC `toolExecution.ts:1457-1464,1726-1741`:
                        // only an ordinary-subagent success omits raw output;
                        // every failure retains its `Error: ...` `toolUseResult`.
                        let preserve_raw = status != ToolResultStatus::Success
                            || context.agent_id.is_none()
                            || context.preserve_tool_use_results;
                        let tool_use_result = preserve_raw
                            .then(|| raw_override.or_else(|| tool.tool_use_result(&result.data)))
                            .flatten();
                        LocalToolExecutionMessageResult {
                            message: Some(
                                RenderableMessage::user_tool_result(
                                    uuid::Uuid::new_v4().to_string(),
                                    request.tool_use_id.clone(),
                                    content,
                                    status != ToolResultStatus::Success,
                                )
                                .with_tool_use_result(tool_use_result),
                            ),
                            new_messages,
                            cwd_after_update,
                            structured_output_update,
                            hook_tool_response,
                            model_content_blocks,
                            context_modifier,
                        }
                    }
                    // Maps to: CC dynamic MCP tools returned from
                    // `fetchToolsForClient(...)`: they are not part of the
                    // static built-in registry, but still execute in the normal
                    // tool-execution flow using the live MCP AppState snapshot.
                    None => {
                        if let Some(message) = dynamic_mcp_tool_result(request, context).await {
                            LocalToolExecutionMessageResult {
                                message: Some(message),
                                ..Default::default()
                            }
                        } else {
                            // Maps to: CC `runToolUse` unknown-tool handling —
                            // a name absent from both the registry and dynamic
                            // MCP state is not a tool.
                            LocalToolExecutionMessageResult {
                                message: Some(tool_error_result(
                                    request,
                                    format!(
                                        "<tool_use_error>Error: No such tool available: {}</tool_use_error>",
                                        request.tool_name
                                    ),
                                )),
                                ..Default::default()
                            }
                        }
                    }
                }
            }
            PermissionPromptChoice::Deny => {
                // Maps to: CC `interactiveHandler.ts:183-203` `onReject(feedback,
                // contentBlocks)` → `resolveOnce(ctx.cancelAndAbort(feedback,
                // undefined, contentBlocks))`. A dialog answer carries no
                // `decision_message` (`types/permissions.rs#decision_message`),
                // and that is the one path where CC replaces the decision — and
                // aborts the controller unless this is a subagent.
                //
                // With a `decision_message` the permission SYSTEM decided and no
                // dialog ran, so CC returns that deny untouched
                // (`toolExecution.ts:1023`) and nothing aborts.
                if response.decision_message.is_none() {
                    crate::hooks::tool_permission::permission_context::cancel_and_abort(
                        &request.tool_name,
                        context,
                        response.feedback.as_deref(),
                        false,
                        &response.content_blocks,
                    );
                }
                LocalToolExecutionMessageResult {
                    message: Some(permission_terminal_result_for_decision(
                        request,
                        ToolResultStatus::Rejected,
                        response.feedback.as_deref(),
                        response.decision_message.as_deref(),
                        context.agent_id.is_some(),
                    )),
                    ..Default::default()
                }
            }
        }
    }
}

/// Maps to: CC `FileReadTool` / `BashTool.mapToolResultToToolResultBlockParam`
/// multimodal branches — image (and Bash structuredContent) bytes live inside
/// `tool_result.content`, not as sibling user messages. PDF document/page
/// images still ride `newMessages` upstream.
fn model_content_blocks_from_tool_output(
    data: &crate::tool::ToolOutput,
) -> Vec<crate::types::message::ToolResultContentBlock> {
    match data {
        crate::tool::ToolOutput::Bash(output) => {
            if let Some(blocks) = output.structured_content.as_ref().filter(|b| !b.is_empty()) {
                return blocks
                    .iter()
                    .map(crate::types::message::ToolResultContentBlock::from_structured_value)
                    .collect();
            }
            if output.is_image {
                if let Some(parsed) = crate::tools::bash_tool::utils::parse_data_uri(&output.stdout)
                {
                    return vec![crate::types::message::ToolResultContentBlock::image_base64(
                        parsed.media_type,
                        parsed.data,
                    )];
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn attach_model_content_blocks(
    tool_result: &mut Option<UserMessage>,
    blocks: Vec<crate::types::message::ToolResultContentBlock>,
) {
    if blocks.is_empty() {
        return;
    }
    let Some(message) = tool_result.as_mut() else {
        return;
    };
    for content in &mut message.content {
        if let UserContent::ToolResult(result) = content {
            // Prefer mapper-owned multimodal blocks over any prior blocks
            // (e.g. ToolSearch tool_reference) — FileRead image is exclusive.
            result.content_blocks = blocks;
            // CC image tool_result content is the image array only; clear the
            // transcript-facing string so API serialization stays image-only.
            result.content.clear();
            return;
        }
    }
}

#[cfg(test)]
mod executor_tests {
    use super::*;
    use crate::tools::tool_search_tool::tool_search_matches;
    use crate::types::message::{ReadResultKind, SearchResultMode};
    use crate::types::permissions::PermissionMode;
    use crate::utils::cron_tasks::reset_cron_tasks_for_test;
    use crate::utils::permissions::permissions::{
        mock_permission_request, mock_permission_request_with_input,
    };
    use crate::utils::swarm::team_helpers::TEAM_TOOL_STATE;
    use crate::utils::tasks::TASK_TOOL_STORE;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct CwdStateGuard {
        process_cwd: std::path::PathBuf,
        original_cwd: std::path::PathBuf,
        worktree_session: Option<crate::utils::worktree::WorktreeSession>,
    }

    impl CwdStateGuard {
        fn capture() -> Self {
            Self {
                process_cwd: std::env::current_dir().expect("current cwd"),
                original_cwd: crate::bootstrap::state::get_original_cwd(),
                worktree_session: crate::utils::worktree::get_current_worktree_session(),
            }
        }
    }

    impl Drop for CwdStateGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.process_cwd);
            crate::bootstrap::state::set_original_cwd(self.original_cwd.clone());
            crate::utils::worktree::restore_worktree_session(self.worktree_session.clone());
        }
    }

    #[tokio::test]
    async fn local_tool_executor_runs_bash_command_after_permission() {
        let request = mock_permission_request_with_input(
            "perm-bash".to_string(),
            "toolu_bash".to_string(),
            "Bash".to_string(),
            "printf cometix-shell-ok".to_string(),
            serde_json::json!({"command": "printf cometix-shell-ok"}),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("bash should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_bash");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert_eq!(block.content, "cometix-shell-ok");
                // The row carries no display shape; the raw
                // `toolUseResult` is the render source. CC `stripEmptyLines`
                // removes the shell's trailing empty line before both model
                // and wire mapping.
                let raw = block.tool_use_result.as_ref().expect("bash raw rides");
                assert_eq!(raw["stdout"], serde_json::json!("cometix-shell-ok"));
                assert_eq!(raw["stderr"], serde_json::json!(""));
            }
            None => panic!("unexpected bash result: {:?}", message.kind),
        }
    }

    #[cfg(feature = "mcp_runtime")]
    #[test]
    fn local_tool_executor_runs_dynamic_mcp_tool_against_live_stdio_client() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let script_path = std::env::temp_dir().join(format!(
            "cometix-dynamic-mcp-tool-{}.mjs",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            r#"
import readline from 'node:readline'
const rl = readline.createInterface({ input: process.stdin })
function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}
rl.on('line', line => {
  let message
  try { message = JSON.parse(line) } catch { return }
  if (message.id === undefined) return
  if (message.method === 'initialize') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        protocolVersion: '2025-11-25',
        capabilities: { tools: {} },
        serverInfo: { name: 'dynamic-tool-fixture', version: '1.0.0' }
      }
    })
  } else if (message.method === 'tools/list') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        tools: [{
          name: 'echo',
          description: 'Echoes the query and tool use id',
          inputSchema: {
            type: 'object',
            properties: { query: { type: 'string' } },
            additionalProperties: false
          }
        }]
      }
    })
  } else if (message.method === 'tools/call') {
    send({
      jsonrpc: '2.0',
      id: message.id,
      result: {
        content: [{
          type: 'text',
          text: `echo:${message.params.arguments?.query}:${message.params._meta?.['claudecode/toolUseId']}`
        }]
      }
    })
  } else {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } })
  }
})
"#,
        )
        .expect("write MCP fixture");

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _ = crate::services::mcp::client::drain_mcp_connection_callback_observations()
                    .await;
                const SERVER: &str = "dynamic-mcp-stdio-fixture";
                let config = crate::services::mcp::types::ScopedMcpServerConfig {
                    name: None,
                    scope: crate::services::mcp::types::ConfigScope::User,
                    transport: crate::services::mcp::types::Transport::Stdio,
                    command: Some("node".to_string()),
                    args: vec![script_path.to_string_lossy().to_string()],
                    env: std::collections::BTreeMap::new(),
                    url: None,
                    headers: std::collections::BTreeMap::new(),
                    headers_helper: None,
                    oauth: None,
                    ide_running_in_windows: None,
                    ide_name: None,
                    auth_token: None,
                    id: None,
                    plugin_source: None,
                };
                let discovery =
                    crate::services::mcp::client::connect_to_server(SERVER, &config).await;
                assert_eq!(
                    discovery.server.client.status,
                    crate::services::mcp::types::McpServerConnectionType::Connected
                );
                assert_eq!(discovery.server.tools.len(), 1);

                let tool_name =
                    crate::services::mcp::mcp_string_utils::build_mcp_tool_name(SERVER, "echo");
                let request = mock_permission_request_with_input(
                    "perm-dynamic-mcp".to_string(),
                    "toolu_dynamic_mcp".to_string(),
                    tool_name.clone(),
                    "query=docs".to_string(),
                    serde_json::json!({"query": "docs"}),
                    PermissionMode::Default,
                );
                let context = crate::tool::ToolUseContext::default().with_mcp_state({
                    let mut state = crate::state::app_state_store::McpState {
                        clients: vec![discovery.server],
                        ..crate::state::app_state_store::McpState::default()
                    };
                    crate::services::mcp::client::refresh_flat_mcp_capabilities(&mut state);
                    state
                });

                let message = LocalToolExecutor
                    .result_after_permission(
                        &request,
                        PermissionPromptChoice::AllowOnce,
                        &context,
                        None,
                        None,
                    )
                    .await
                    .expect("dynamic MCP tool should produce a result");

                // The row no longer stores a tool name: the renderer
                // resolves it via lookups, so the old name assertion is gone.
                match user_tool_result_block(&message) {
                    Some(block) => {
                        assert_eq!(block.tool_use_id.0, "toolu_dynamic_mcp");
                        assert_eq!(block.derived_status(), ToolResultStatus::Success);
                        assert_eq!(block.content, "echo:docs:toolu_dynamic_mcp");
                        // No display shape — the raw content-blocks
                        // value rides the row (`data: mcpResult.content`).
                        assert_eq!(
                            block.tool_use_result,
                            Some(serde_json::json!([{
                                "type": "text",
                                "text": "echo:docs:toolu_dynamic_mcp"
                            }]))
                        );
                    }
                    None => panic!("unexpected dynamic MCP result: {:?}", message.kind),
                }

                crate::services::mcp::client::clear_server_cache(SERVER, None).await;
                let _ = crate::services::mcp::client::drain_mcp_connection_callback_observations()
                    .await;
            });
        let _ = std::fs::remove_file(script_path);
    }

    #[tokio::test]
    async fn local_tool_executor_forwards_parent_assistant_message_to_tool_call() {
        let request = mock_permission_request_with_input(
            "perm-parent-probe".to_string(),
            "toolu_parent_probe".to_string(),
            "ParentMessageProbe".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_parent_probe".to_string()),
                    name: "ParentMessageProbe".to_string(),
                    input: serde_json::json!({}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                Some(&assistant_message),
                None,
            )
            .await
            .expect("parent probe should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.content, "parent=true,can_use_tool=true");
            }
            None => panic!("unexpected parent probe result: {:?}", message.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_times_out_bash_command() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _background = EnvVarGuard::set("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        let request = mock_permission_request_with_input(
            "perm-bash-timeout".to_string(),
            "toolu_bash_timeout".to_string(),
            "Bash".to_string(),
            "sleep 1".to_string(),
            serde_json::json!({"command": "sleep 1", "timeout": 20}),
            PermissionMode::Default,
        );

        let started = std::time::Instant::now();
        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("bash timeout should produce a tool_result");

        assert!(started.elapsed() < std::time::Duration::from_millis(800));
        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_bash_timeout");
                assert_eq!(block.derived_status(), ToolResultStatus::Error);
                // CC ShellCommand resolves timeout with SIGTERM_EXIT (143),
                // not SIGKILL, so `interrupted` remains false and no abort tag
                // is added by BashTool's model mapper.
                assert!(
                    !block
                        .content
                        .contains("Command was aborted before completion")
                );
                assert_eq!(block.content, "Exit code 143");
                // No display shape; semantic errors keep the synthetic
                // timeout stderr out of the raw wire value too.
                let raw = block.tool_use_result.as_ref().expect("bash raw rides");
                assert_eq!(raw["stderr"], serde_json::json!(""));
            }
            None => panic!("unexpected bash timeout result: {:?}", message.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_runs_background_bash_and_task_output() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store);
        let start_request = mock_permission_request_with_input(
            "perm-bash-background".to_string(),
            "toolu_bash_background".to_string(),
            "Bash".to_string(),
            "printf background-ok".to_string(),
            serde_json::json!({
                "command": "printf background-ok",
                "description": "print background marker",
                "run_in_background": true
            }),
            PermissionMode::Default,
        );

        let started = LocalToolExecutor
            .result_after_permission(
                &start_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await
            .expect("background bash should produce task id");
        let task_id = match user_tool_result_block(&started) {
            Some(block) => block
                .content
                .split("Command running in background with ID: ")
                .nth(1)
                .map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or(rest)
                        .trim_end_matches('.')
                })
                .expect("background task id should be present")
                .to_string(),
            None => panic!("unexpected background start result: {:?}", started.kind),
        };

        let output_request = mock_permission_request_with_input(
            "perm-task-output".to_string(),
            "toolu_task_output".to_string(),
            "TaskOutput".to_string(),
            task_id.clone(),
            serde_json::json!({"task_id": task_id, "block": true, "timeout": 1000}),
            PermissionMode::Default,
        );
        let output = LocalToolExecutor
            .result_after_permission(
                &output_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await
            .expect("task output should produce a tool_result");

        match user_tool_result_block(&output) {
            Some(block) => {
                assert_eq!(
                    block.derived_status(),
                    ToolResultStatus::Success,
                    "unexpected TaskOutput result: content={:?}",
                    block.content
                );
                assert!(
                    block
                        .content
                        .contains("<retrieval_status>success</retrieval_status>")
                );
                assert!(block.content.contains("background-ok"));
                // No display shape — the raw Output object rides the row.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("raw output should ride the row");
                assert_eq!(
                    raw.get("retrieval_status"),
                    Some(&serde_json::json!("success"))
                );
                assert!(
                    raw.pointer("/task/output")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|output| output.contains("background-ok"))
                );
            }
            None => panic!("unexpected task output result: {:?}", output.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_stops_background_bash_task() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = crate::tool::ToolUseContext::default().with_app_store(store);
        let start_request = mock_permission_request_with_input(
            "perm-bash-stop-background".to_string(),
            "toolu_bash_stop_background".to_string(),
            "Bash".to_string(),
            "sleep 5".to_string(),
            serde_json::json!({
                "command": "sleep 5",
                "description": "sleep briefly",
                "run_in_background": true
            }),
            PermissionMode::Default,
        );

        let started = LocalToolExecutor
            .result_after_permission(
                &start_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await
            .expect("background bash should produce task id");
        let task_id = match user_tool_result_block(&started) {
            Some(block) => block
                .content
                .split("Command running in background with ID: ")
                .nth(1)
                .map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or(rest)
                        .trim_end_matches('.')
                })
                .expect("background task id should be present")
                .to_string(),
            None => panic!("unexpected background start result: {:?}", started.kind),
        };

        let stop_request = mock_permission_request_with_input(
            "perm-task-stop".to_string(),
            "toolu_task_stop".to_string(),
            "TaskStop".to_string(),
            task_id.clone(),
            serde_json::json!({"task_id": task_id}),
            PermissionMode::Default,
        );
        let stopped = LocalToolExecutor
            .result_after_permission(
                &stop_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await
            .expect("task stop should produce a tool_result");

        match user_tool_result_block(&stopped) {
            Some(block) => {
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(block.content.contains("Successfully stopped task"));
                // No display shape — the raw Output object rides the row.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("raw output should ride the row");
                assert_eq!(
                    raw.get("command").and_then(serde_json::Value::as_str),
                    Some("sleep 5")
                );
                // The other required Output fields ride the wire too.
                assert!(raw.get("message").is_some());
                assert!(raw.get("task_id").is_some());
                assert!(raw.get("task_type").is_some());
            }
            None => panic!("unexpected task stop result: {:?}", stopped.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_reads_file_after_permission() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _model = EnvVarGuard::set("ANTHROPIC_MODEL", "claude-sonnet-4-6");
        let path =
            std::env::temp_dir().join(format!("cometix-read-tool-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, "line one\nline two\n").unwrap();
        let request = mock_permission_request_with_input(
            "perm-read".to_string(),
            "toolu_read".to_string(),
            "Read".to_string(),
            path.to_string_lossy().to_string(),
            serde_json::json!({"file_path": path.to_string_lossy()}),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("read should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_read");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                // CC-parity content: `addLineNumbers` compact format
                // (utils/file.ts:290 via FileReadTool map :698).
                assert_eq!(
                    block.content,
                    format!(
                        "1\tline one\n2\tline two\n3\t{}",
                        crate::tools::file_read_tool::CYBER_RISK_MITIGATION_REMINDER
                    )
                );
                // The row carries the raw `toolUseResult` (CC's shape);
                // the display holds nothing Read-specific.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("read row records its raw output");
                assert_eq!(raw["type"], "text");
                assert_eq!(raw["file"]["numLines"], 3);
            }
            None => panic!("unexpected read result: {:?}", message.kind),
        }

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn read_subagent_raw_tool_use_result_policy_matches_official() {
        let context = ToolUseContext {
            agent_id: Some("agent-read-raw".to_string()),
            ..ToolUseContext::default()
        };
        let invalid_request = mock_permission_request_with_input(
            "perm-read-invalid".to_string(),
            "toolu_read_invalid".to_string(),
            "Read".to_string(),
            "invalid path".to_string(),
            serde_json::json!({"file_path": 7}),
            PermissionMode::Default,
        );
        let invalid = check_permissions_and_call_tool_async(
            &invalid_request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;
        let invalid_result = invalid
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
            .and_then(|content| match content {
                UserContent::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("subagent Read validation result");
        assert!(invalid_result.is_error);
        assert!(
            matches!(
                invalid_result.tool_use_result.as_ref(),
                Some(serde_json::Value::String(error)) if !error.is_empty()
            ),
            "toolUseResult={:?}",
            invalid_result.tool_use_result
        );

        let path = std::env::temp_dir().join(format!(
            "cometix-read-subagent-raw-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "one line\n").unwrap();
        let success_request = mock_permission_request_with_input(
            "perm-read-success".to_string(),
            "toolu_read_success".to_string(),
            "Read".to_string(),
            path.display().to_string(),
            serde_json::json!({"file_path": path}),
            PermissionMode::Default,
        );
        let rejected_message = permission_terminal_result(
            &success_request,
            ToolResultStatus::Rejected,
            context.agent_id.is_some(),
        );
        let rejected_model = transcript_tool_result_to_model_message(&rejected_message, "Read")
            .expect("subagent Read permission rejection");
        // Re-derived 2026-08-26 (#135): this row is a SUBAGENT rejection
        // (`agent_id: Some(...)` above), and CC `PermissionContext.ts:159-164`
        // reads `const sub = !!toolUseContext.agentId` before picking the copy,
        // so the model sees `SUBAGENT_REJECT_MESSAGE`
        // (`utils/messages.ts:216-217`), not `REJECT_MESSAGE`. The assertion had
        // transcribed the port, which had no `sub` branch at all. The subject
        // under test here is the raw `toolUseResult` policy, which is unchanged:
        // `Error: ${errorMessage}` (`toolExecution.ts:1068`).
        assert!(matches!(
            rejected_model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.tool_use_result == Some(serde_json::Value::String(format!(
                    "Error: {}",
                    crate::utils::messages::SUBAGENT_REJECT_MESSAGE
                )))
        ));

        let success = check_permissions_and_call_tool_async(
            &success_request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;
        let success_result = success
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
            .and_then(|content| match content {
                UserContent::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("subagent Read success result");
        assert!(!success_result.is_error);
        assert!(success_result.tool_use_result.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn read_listener_runs_only_after_token_validation_succeeds() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _api_key = EnvVarGuard::unset("ANTHROPIC_API_KEY");
        let _auth_token = EnvVarGuard::unset("ANTHROPIC_AUTH_TOKEN");
        let _oauth_token = EnvVarGuard::unset("CLAUDE_CODE_OAUTH_TOKEN");
        let _bedrock = EnvVarGuard::unset("CLAUDE_CODE_USE_BEDROCK");
        let _vertex = EnvVarGuard::unset("CLAUDE_CODE_USE_VERTEX");
        let _foundry = EnvVarGuard::unset("CLAUDE_CODE_USE_FOUNDRY");
        let path = std::env::temp_dir().join(format!(
            "cometix-read-listener-token-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "listener-content-".repeat(32)).unwrap();
        let request = mock_permission_request_with_input(
            "perm-read-listener".to_string(),
            "toolu_read_listener".to_string(),
            "Read".to_string(),
            path.display().to_string(),
            serde_json::json!({"file_path": path}),
            PermissionMode::Default,
        );
        let notifications = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&notifications);
        let unsubscribe = crate::tools::file_read_tool::register_file_read_listener(
            std::sync::Arc::new(move |path, content| {
                captured
                    .lock()
                    .unwrap()
                    .push((path.to_string(), content.to_string()));
                Ok(())
            }),
        );

        let mut too_small = ToolUseContext::default();
        too_small.file_reading_limits = Some(crate::tool::FileReadingLimitsOverride {
            max_tokens: Some(1.0),
            max_size_bytes: None,
        });
        let rejected = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &too_small,
            None,
        )
        .await;
        let Some(UserContent::ToolResult(rejected_result)) = rejected
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected token rejection")
        };
        assert!(rejected_result.is_error);
        assert!(
            rejected_result
                .content
                .contains("exceeds maximum allowed tokens")
        );
        assert!(notifications.lock().unwrap().is_empty());
        assert!(rejected.new_context.read_file_state.is_empty());

        let mut accepted_context = ToolUseContext::default();
        accepted_context.file_reading_limits = Some(crate::tool::FileReadingLimitsOverride {
            max_tokens: Some(10_000.0),
            max_size_bytes: None,
        });
        let accepted = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &accepted_context,
            None,
        )
        .await;
        assert!(
            accepted
                .tool_result
                .as_ref()
                .and_then(|message| message.content.first())
                .is_some_and(
                    |content| matches!(content, UserContent::ToolResult(result) if !result.is_error)
                )
        );
        assert_eq!(accepted.new_context.read_file_state.len(), 1);
        let notifications = notifications.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, path.display().to_string());
        assert!(notifications[0].1.starts_with("listener-content-"));
        drop(notifications);
        unsubscribe();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn read_image_puts_bytes_inside_tool_result_content_blocks() {
        // 1×1 PNG
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D,
            0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let path =
            std::env::temp_dir().join(format!("cometix-read-image-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, TINY_PNG).unwrap();
        let path_str = path.display().to_string();

        let request = mock_permission_request_with_input(
            "perm-read-img".to_string(),
            "toolu_read_img".to_string(),
            "Read".to_string(),
            path_str.clone(),
            serde_json::json!({ "file_path": path_str }),
            PermissionMode::Default,
        );
        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &crate::tool::ToolUseContext::default(),
            None,
        )
        .await;

        let tool_result = result.tool_result.expect("model tool_result");
        let UserContent::ToolResult(block) = &tool_result.content[0] else {
            panic!("expected ToolResult content");
        };
        assert!(
            block.content.is_empty(),
            "CC image tool_result content is the image array only"
        );
        assert_eq!(block.content_blocks.len(), 1);
        match &block.content_blocks[0] {
            crate::types::message::ToolResultContentBlock::Image { source } => {
                assert_eq!(source.kind, "base64");
                assert!(source.media_type.starts_with("image/"));
                assert!(!source.data.is_empty());
            }
            other => panic!("expected Image content block, got {other:?}"),
        }
        // Image must not also be duplicated as a sibling user message.
        assert!(
            !result.new_messages.iter().any(|message| {
                matches!(
                    message,
                    Message::User(user)
                        if user.content.iter().any(|c| matches!(c, UserContent::Image { .. }))
                )
            }),
            "image should not ride newMessages"
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn read_mapper_failure_retains_call_effects_without_new_messages() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-mapper-failure-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("future.ipynb");
        std::fs::write(
            &path,
            r#"{"metadata":{"language_info":{"name":"python"}},"cells":[{"cell_type":"code","source":["1"],"outputs":[{"output_type":"future_output"}]}]}"#,
        )
        .unwrap();
        let path_str = path.display().to_string();
        let request = mock_permission_request_with_input(
            "perm-read-mapper-failure".to_string(),
            "toolu_read_mapper_failure".to_string(),
            "Read".to_string(),
            path_str.clone(),
            serde_json::json!({ "file_path": path_str }),
            PermissionMode::Default,
        );
        let context = ToolUseContext {
            cwd_override: Some(root.clone()),
            ..ToolUseContext::default()
        };

        let result = check_permissions_and_call_tool_async(
            &request,
            PermissionPromptChoice::AllowOnce,
            &context,
            None,
        )
        .await;

        let Some(UserContent::ToolResult(block)) = result
            .tool_result
            .as_ref()
            .and_then(|message| message.content.first())
        else {
            panic!("expected mapper failure tool result")
        };
        assert!(block.is_error);
        assert!(
            block
                .content
                .contains("Cannot read properties of undefined")
        );
        assert!(result.new_messages.is_empty());
        assert!(result.new_context.read_file_state.has(&path));
        assert!(
            result
                .new_context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains(&path.display().to_string())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn read_then_write_updates_live_read_file_state_like_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let path = std::env::temp_dir().join(format!(
            "cometix-read-write-state-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "old\n").unwrap();
        let path_str = path.display().to_string();

        let read_request = mock_permission_request_with_input(
            "perm-rws-read".to_string(),
            "toolu_rws_read".to_string(),
            "Read".to_string(),
            path_str.clone(),
            serde_json::json!({ "file_path": path_str }),
            PermissionMode::Default,
        );
        let read_result = check_permissions_and_call_tool_async(
            &read_request,
            PermissionPromptChoice::AllowOnce,
            &crate::tool::ToolUseContext::default(),
            None,
        )
        .await;
        assert!(
            read_result
                .new_context
                .read_file_state
                .snapshot()
                .into_iter()
                .any(|entry| {
                    entry.path == path_str && entry.content.as_deref() == Some("old\n")
                }),
            "Read should seed live readFileState"
        );

        let write_request = mock_permission_request_with_input(
            "perm-rws-write".to_string(),
            "toolu_rws_write".to_string(),
            "Write".to_string(),
            path_str.clone(),
            serde_json::json!({
                "file_path": path_str,
                "content": "new\n"
            }),
            PermissionMode::Default,
        );
        let write_result = check_permissions_and_call_tool_async(
            &write_request,
            PermissionPromptChoice::AllowOnce,
            &read_result.new_context,
            None,
        )
        .await;
        match write_result
            .message
            .as_ref()
            .and_then(user_tool_result_block)
        {
            Some(block) => {
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
            }
            None => panic!(
                "expected write success, got {:?}",
                write_result.message.as_ref().map(|m| &m.kind)
            ),
        }
        let entry = write_result
            .new_context
            .read_file_state
            .snapshot()
            .into_iter()
            .find(|entry| entry.path == path_str)
            .expect("Write should refresh readFileState");
        assert_eq!(entry.content.as_deref(), Some("new\n"));
        assert_eq!(
            entry.source,
            crate::utils::query_helpers::ReadFileStateSource::Write
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn local_tool_executor_runs_glob_and_grep_readonly_tools() {
        let root =
            std::env::temp_dir().join(format!("cometix-search-tools-{}", uuid::Uuid::new_v4()));
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("main.rs"),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "hello docs\n").unwrap();

        let glob_request = mock_permission_request(
            "perm-glob".to_string(),
            "toolu_glob".to_string(),
            "Glob".to_string(),
            serde_json::json!({
                "pattern": "*.rs",
                "path": nested.to_string_lossy(),
            })
            .to_string(),
            PermissionMode::Default,
        );
        let glob_message = LocalToolExecutor
            .result_after_permission(
                &glob_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("glob should produce a tool_result");
        match user_tool_result_block(&glob_message) {
            Some(crate::types::message::ToolResult {
                is_error: false,
                content,
                // No Glob display shape — the raw rides the row.
                tool_use_result: Some(raw),
                ..
            }) => {
                assert_eq!(raw["numFiles"], serde_json::json!(1));
                assert_eq!(raw["truncated"], serde_json::json!(false));
                assert!(content.contains("main.rs"));
            }
            other => panic!("unexpected glob result: {other:?}"),
        }

        let agent_context = crate::tool::ToolUseContext {
            agent_id: Some("agent-glob".to_string()),
            ..crate::tool::ToolUseContext::default()
        };
        let agent_glob_message = LocalToolExecutor
            .result_after_permission(
                &glob_request,
                PermissionPromptChoice::AllowOnce,
                &agent_context,
                None,
                None,
            )
            .await
            .expect("agent Glob result");
        // Persistence gates the raw value itself — an ordinary subagent
        // success omits it from the row.
        assert!(matches!(
            user_tool_result_block(&agent_glob_message),
            Some(block) if block.tool_use_result.is_none()
        ));
        let agent_model = transcript_tool_result_to_model_message(&agent_glob_message, "Glob")
            .expect("agent model result");
        assert!(matches!(
            agent_model.content.first(),
            Some(UserContent::ToolResult(result)) if result.tool_use_result.is_none()
        ));

        let preserving_agent_context = crate::tool::ToolUseContext {
            agent_id: Some("agent-glob".to_string()),
            preserve_tool_use_results: true,
            ..crate::tool::ToolUseContext::default()
        };
        let preserved = LocalToolExecutor
            .result_after_permission(
                &glob_request,
                PermissionPromptChoice::AllowOnce,
                &preserving_agent_context,
                None,
                None,
            )
            .await
            .expect("preserved agent Glob result");
        let preserved_model = transcript_tool_result_to_model_message(&preserved, "Glob")
            .expect("preserved model result");
        assert!(matches!(
            preserved_model.content.first(),
            Some(UserContent::ToolResult(result)) if result.tool_use_result.is_some()
        ));

        let grep_request = mock_permission_request(
            "perm-grep".to_string(),
            "toolu_grep".to_string(),
            "Grep".to_string(),
            serde_json::json!({
                "pattern": "hello",
                "path": root.to_string_lossy(),
                "glob": "*.rs",
                "output_mode": "content"
            })
            .to_string(),
            PermissionMode::Default,
        );
        let grep_message = LocalToolExecutor
            .result_after_permission(
                &grep_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("grep should produce a tool_result");
        let grep_model = transcript_tool_result_to_model_message(&grep_message, "Grep")
            .expect("Grep result maps back to model history");
        assert!(matches!(
            grep_model.content.first(),
            Some(UserContent::ToolResult(result))
                if matches!(result.tool_use_result.as_ref(), Some(serde_json::Value::Object(raw))
                    if raw.get("mode") == Some(&serde_json::json!("content"))
                        && raw.get("numFiles") == Some(&serde_json::json!(0))
                        && raw.get("filenames") == Some(&serde_json::json!([]))
                        && raw.get("numLines") == Some(&serde_json::json!(1))
                        && raw.get("content").and_then(serde_json::Value::as_str)
                            .is_some_and(|content| content.contains("main.rs")))
        ));
        match user_tool_result_block(&grep_message) {
            Some(crate::types::message::ToolResult {
                is_error: false,
                content,
                // No Grep display shape — the raw asserted on the model
                // projection above is the same value riding the row.
                tool_use_result: Some(raw),
                ..
            }) => {
                assert_eq!(raw["mode"], serde_json::json!("content"));
                assert_eq!(raw["numLines"], serde_json::json!(1));
                // CC parity: content mode emits `filenames: []`
                // (GrepTool.ts:466-474); the match location lives in the
                // `path:line:text` content rows instead.
                assert_eq!(raw["filenames"], serde_json::json!([]));
                assert!(content.contains("main.rs"));
                assert!(content.contains("println!"));
                assert!(!content.contains("README.md"));
            }
            other => panic!("unexpected grep result: {other:?}"),
        }

        let agent_grep = LocalToolExecutor
            .result_after_permission(
                &grep_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext {
                    agent_id: Some("agent-grep".to_string()),
                    ..crate::tool::ToolUseContext::default()
                },
                None,
                None,
            )
            .await
            .expect("agent Grep result");
        // Persistence gates the raw value itself — an ordinary subagent
        // success omits it from the row, and the display stays Generic.
        assert!(matches!(
            user_tool_result_block(&agent_grep),
            Some(block) if block.tool_use_result.is_none()
        ));
        let agent_model = transcript_tool_result_to_model_message(&agent_grep, "Grep")
            .expect("agent Grep model result");
        assert!(matches!(
            agent_model.content.first(),
            Some(UserContent::ToolResult(result)) if result.tool_use_result.is_none()
        ));

        let preserved_grep = LocalToolExecutor
            .result_after_permission(
                &grep_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext {
                    agent_id: Some("agent-grep".to_string()),
                    preserve_tool_use_results: true,
                    ..crate::tool::ToolUseContext::default()
                },
                None,
                None,
            )
            .await
            .expect("preserved agent Grep result");
        let preserved_model = transcript_tool_result_to_model_message(&preserved_grep, "Grep")
            .expect("preserved Grep model result");
        assert!(matches!(
            preserved_model.content.first(),
            Some(UserContent::ToolResult(result)) if result.tool_use_result.is_some()
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn local_tool_executor_read_honors_structured_offset_and_limit_input() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _model = EnvVarGuard::set("ANTHROPIC_MODEL", "claude-sonnet-4-6");
        let path =
            std::env::temp_dir().join(format!("cometix-read-window-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();
        let request = mock_permission_request_with_input(
            "perm-read-window".to_string(),
            "toolu_read_window".to_string(),
            "Read".to_string(),
            path.to_string_lossy().to_string(),
            serde_json::json!({
                "file_path": path.to_string_lossy(),
                "offset": 2,
                "limit": 2
            }),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("read should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(
                    block.content,
                    format!(
                        "2\ttwo\n3\tthree{}",
                        crate::tools::file_read_tool::CYBER_RISK_MITIGATION_REMINDER
                    )
                );
            }
            None => panic!("unexpected read result: {:?}", message.kind),
        }

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn local_tool_executor_writes_and_edits_files_after_permission() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root =
            std::env::temp_dir().join(format!("cometix-write-edit-{}", uuid::Uuid::new_v4()));
        let path = root.join("nested").join("file.txt");
        let mut context = crate::tool::ToolUseContext::default();
        let write_request = mock_permission_request_with_input(
            "perm-write".to_string(),
            "toolu_write".to_string(),
            "Write".to_string(),
            path.to_string_lossy().to_string(),
            serde_json::json!({
                "file_path": path.to_string_lossy(),
                "content": "alpha\nbeta\n"
            }),
            PermissionMode::Default,
        );

        let write_result = LocalToolExecutor
            .result_after_permission_with_new_messages(
                &write_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await;
        let write_message = write_result
            .message
            .expect("write should produce a tool_result");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
        match user_tool_result_block(&write_message) {
            Some(result) if !result.is_error => {
                // The raw `toolUseResult` is the render source.
                let output = result
                    .tool_use_result
                    .as_ref()
                    .and_then(crate::tools::file_write_tool::ui::parse_output)
                    .expect("write raw rides");
                assert!(matches!(
                    output.kind,
                    crate::tools::file_write_tool::WriteOutputKind::Create
                ));
                assert_eq!(
                    crate::tools::file_write_tool::ui::count_lines(&output.content),
                    2
                );
            }
            other => panic!("unexpected write result: {other:?}"),
        }

        let edit_request = mock_permission_request_with_input(
            "perm-edit".to_string(),
            "toolu_edit".to_string(),
            "Edit".to_string(),
            path.to_string_lossy().to_string(),
            serde_json::json!({
                "file_path": path.to_string_lossy(),
                "old_string": "beta",
                "new_string": "gamma"
            }),
            PermissionMode::Default,
        );
        let edit_message = LocalToolExecutor
            .result_after_permission(
                &edit_request,
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                None,
            )
            .await
            .expect("edit should produce a tool_result");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
        match user_tool_result_block(&edit_message) {
            Some(result) if !result.is_error => {
                // The raw `toolUseResult` carries the structured patch.
                let output = result
                    .tool_use_result
                    .as_ref()
                    .and_then(crate::tools::file_edit_tool::ui::parse_output)
                    .expect("edit raw rides");
                let diff_lines = output
                    .structured_patch
                    .iter()
                    .flat_map(|hunk| hunk.lines.iter())
                    .collect::<Vec<_>>();
                assert!(diff_lines.iter().any(|line| line.as_str() == "-beta"));
                assert!(diff_lines.iter().any(|line| line.as_str() == "+gamma"));
            }
            other => panic!("unexpected edit result: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn local_tool_executor_plan_mode_tools_return_official_tool_results() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("cometix-exit-plan-tool-{}", uuid::Uuid::new_v4()));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let expected_plan_path = crate::utils::plans::get_plan_file_path(None);

        let enter_request = mock_permission_request_with_input(
            "perm-enter-plan".to_string(),
            "toolu_enter_plan".to_string(),
            "EnterPlanMode".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let enter_message = LocalToolExecutor
            .result_after_permission(
                &enter_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("EnterPlanMode should produce a tool_result");
        match user_tool_result_block(&enter_message) {
            Some(crate::types::message::ToolResult {
                is_error: false,
                content,
                // No display shape — the raw Output object rides the row.
                tool_use_result,
                ..
            }) => {
                // CC `EnterPlanModeTool.ts:103-110` branches the instructions
                // on `isPlanModeInterviewPhaseEnabled()` — always true for ant
                // builds (`planModeV2.ts:50-52`, a USER_TYPE build define),
                // GrowthBook-default false externally. This used to pin only
                // the external copy.
                if crate::utils::plan_mode_v2::is_plan_mode_interview_phase_enabled() {
                    assert!(content.contains(
                        "DO NOT write or edit any files except the plan file. \
                         Detailed workflow instructions will follow."
                    ));
                } else {
                    assert!(content.contains("In plan mode, you should:"));
                    assert!(content.contains("Remember: DO NOT write or edit any files yet"));
                }
                assert!(
                    tool_use_result
                        .as_ref()
                        .and_then(|raw| raw.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|message| message.starts_with("Entered plan mode."))
                );
            }
            other => panic!("unexpected EnterPlanMode result: {other:?}"),
        }

        let exit_request = mock_permission_request_with_input(
            "perm-exit-plan".to_string(),
            "toolu_exit_plan".to_string(),
            "ExitPlanMode".to_string(),
            "approve plan".to_string(),
            serde_json::json!({
                "plan": "## Plan\nShip it",
                "planFilePath": "/tmp/project/.claude/plans/plan.md"
            }),
            PermissionMode::Plan,
        );
        let exit_message = LocalToolExecutor
            .result_after_permission(
                &exit_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("ExitPlanMode should produce a tool_result");
        match user_tool_result_block(&exit_message) {
            Some(crate::types::message::ToolResult {
                content,
                // No display shape — the raw Output object rides the row.
                tool_use_result: Some(raw),
                ..
            }) => {
                assert!(content.contains("User has approved your plan"));
                assert!(content.contains("## Approved Plan"));
                assert_eq!(
                    raw.get("plan"),
                    Some(&serde_json::json!("## Plan\nShip it"))
                );
                assert_eq!(
                    raw.get("filePath").and_then(serde_json::Value::as_str),
                    Some(expected_plan_path.to_string_lossy().as_ref())
                );
                assert_eq!(raw.get("isAgent"), Some(&serde_json::json!(false)));
                assert!(raw.get("awaitingLeaderApproval").is_none());
                assert_eq!(
                    std::fs::read_to_string(&expected_plan_path).unwrap(),
                    "## Plan\nShip it"
                );
            }
            other => panic!("unexpected ExitPlanMode result: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(config_home);
    }

    #[tokio::test]
    async fn local_tool_executor_task_v2_tools_use_in_memory_official_model_content() {
        let _task_guard = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _temp = crate::utils::tasks::TempTaskConfig::new("executor-list");
        let create_request = mock_permission_request_with_input(
            "perm-task-create".to_string(),
            "toolu_task_create".to_string(),
            "TaskCreate".to_string(),
            "Review auth".to_string(),
            serde_json::json!({"subject": "Review auth", "description": "Inspect auth flow"}),
            PermissionMode::Default,
        );
        let created = LocalToolExecutor
            .result_after_permission(
                &create_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TaskCreate should produce a tool_result");
        match user_tool_result_block(&created) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_task_create");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert_eq!(block.content, "Task #1 created successfully: Review auth");
                // CC TaskCreateTool.ts defines no renderToolResultMessage —
                // the success row hides at render time by name.
            }
            None => panic!("unexpected TaskCreate result: {:?}", created.kind),
        }

        let list_request = mock_permission_request_with_input(
            "perm-task-list".to_string(),
            "toolu_task_list".to_string(),
            "TaskList".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let listed = LocalToolExecutor
            .result_after_permission(
                &list_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TaskList should produce a tool_result");
        assert!(matches!(
            user_tool_result_block(&listed),
            Some(block) if block.content == "#1 [pending] Review auth"        ));

        let update_request = mock_permission_request_with_input(
            "perm-task-update".to_string(),
            "toolu_task_update".to_string(),
            "TaskUpdate".to_string(),
            "1 completed".to_string(),
            serde_json::json!({"taskId": "1", "status": "completed"}),
            PermissionMode::Default,
        );
        let updated = LocalToolExecutor
            .result_after_permission(
                &update_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TaskUpdate should produce a tool_result");
        assert!(matches!(
            user_tool_result_block(&updated),
            Some(block) if block.content == "Updated task #1 status"        ));

        let get_request = mock_permission_request_with_input(
            "perm-task-get".to_string(),
            "toolu_task_get".to_string(),
            "TaskGet".to_string(),
            "1".to_string(),
            serde_json::json!({"taskId": "1"}),
            PermissionMode::Default,
        );
        let got = LocalToolExecutor
            .result_after_permission(
                &get_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TaskGet should produce a tool_result");
        assert!(matches!(
            user_tool_result_block(&got),
            Some(block) if block.content.contains("Task #1: Review auth")
                && block.content.contains("Status: completed")
                && block.content.contains("Description: Inspect auth flow")        ));
    }

    #[tokio::test]
    async fn local_tool_executor_tool_search_returns_official_output_for_known_tool() {
        let request = mock_permission_request_with_input(
            "perm-tool-search".to_string(),
            "toolu_tool_search".to_string(),
            "ToolSearch".to_string(),
            "select:Read".to_string(),
            serde_json::json!({"query": "select:Read", "max_results": 5}),
            PermissionMode::Default,
        );
        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("ToolSearch should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_tool_search");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                let value: serde_json::Value = serde_json::from_str(&block.content).unwrap();
                assert_eq!(value["query"], serde_json::json!("select:Read"));
                assert_eq!(value["matches"], serde_json::json!(["Read"]));
                assert!(value["total_deferred_tools"].as_u64().unwrap() > 0);
                // CC ToolSearchTool defines NO renderToolResultMessage
                // (ToolSearchTool.ts only has renderToolUseMessage → null
                // :435-437 and userFacingName '' :438), so the success row is
                // invisible (UserToolSuccessMessage.tsx:86-103) — a
                // render-time decision keyed on the tool name.
            }
            None => panic!("unexpected ToolSearch result: {:?}", message.kind),
        }
        assert!(
            !crate::components::messages::user_tool_result_message::transcript_tool_result_should_emit_ui(
                &message,
                Some("ToolSearch"),
            ),
            "ToolSearch success rows must be dropped by the emit-UI gate"
        );
    }

    /// The deferred fixture is an MCP tool because the keyword search scores
    /// `getToolDescriptionMemoized` = `tool.prompt(...)`
    /// (`ToolSearchTool.ts:63-85`, :241/:262), and MCP tools are the family
    /// whose `prompt()` IS the carried server description
    /// (`services/mcp/client.ts:1789-1794`) — the only way a fixture can
    /// control the scoring text. A registry built-in would render its real
    /// registered prompt and ignore the string written here.
    #[test]
    fn tool_search_keyword_search_only_uses_deferred_tools_but_select_can_return_loaded_tool() {
        let tools = vec![
            crate::types::tools::Tool {
                name: "Bash".to_string(),
                description: "unique eager shell command".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                strict: Some(true),
                ..Default::default()
            },
            crate::types::tools::Tool {
                name: "mcp__todo__write".to_string(),
                description: "unique deferred todo update".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                strict: Some(true),
                is_mcp: true,
                ..Default::default()
            },
        ];
        let deferred = tools
            .iter()
            .filter(|tool| crate::tools::tool_search_tool::prompt::is_deferred_tool(tool))
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(
            tool_search_matches("eager", &deferred, &tools, 5),
            Vec::<String>::new()
        );
        assert_eq!(
            tool_search_matches("unique deferred", &deferred, &tools, 5),
            vec!["mcp__todo__write".to_string()]
        );
        assert_eq!(
            tool_search_matches("select:Bash", &deferred, &tools, 5),
            vec!["Bash".to_string()]
        );
    }

    #[tokio::test]
    async fn local_tool_executor_remote_trigger_returns_explicit_501_payload_without_network() {
        let request = mock_permission_request_with_input(
            "perm-remote-trigger".to_string(),
            "toolu_remote_trigger".to_string(),
            "RemoteTrigger".to_string(),
            "list".to_string(),
            serde_json::json!({"action": "list"}),
            PermissionMode::Default,
        );
        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("RemoteTrigger should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_remote_trigger");
                // CC pairs `validateStatus: () => true` with a result block that
                // never sets `is_error`, leaving the status code for the model to
                // read (RemoteTriggerTool.ts:152-158).
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(block.content.starts_with("HTTP 501\n"));
                assert!(block.content.contains("no remote API request was sent"));
                // No display shape — the raw Output object rides the row.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("raw output should ride the row");
                assert_eq!(raw.get("status"), Some(&serde_json::json!(501)));
                assert!(
                    raw.get("json")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(
                            |json| json.contains("Remote trigger execution is unavailable")
                        )
                );
            }
            other => panic!("unexpected RemoteTrigger result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_cron_tools_use_in_memory_official_model_content() {
        reset_cron_tasks_for_test();
        let create_request = mock_permission_request_with_input(
            "perm-cron-create".to_string(),
            "toolu_cron_create".to_string(),
            "CronCreate".to_string(),
            "*/5 * * * *: check deploy".to_string(),
            serde_json::json!({"cron": "*/5 * * * *", "prompt": "check deploy", "recurring": true}),
            PermissionMode::Default,
        );
        let created = LocalToolExecutor
            .result_after_permission(
                &create_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("CronCreate should produce a tool_result");
        // No cron display shapes — the raw rides the row.
        let created_id = match user_tool_result_block(&created) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_cron_create");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(block.content.contains("Scheduled recurring job"));
                assert!(block.content.contains("Every 5 minutes"));
                assert!(block.content.contains("Session-only"));
                let raw = block.tool_use_result.as_ref().expect("raw rides the row");
                assert_eq!(raw["humanSchedule"], serde_json::json!("Every 5 minutes"));
                assert_eq!(raw["recurring"], serde_json::json!(true));
                let id = raw["id"].as_str().expect("id").to_string();
                assert_eq!(id.len(), 8);
                id
            }
            None => panic!("unexpected CronCreate result: {:?}", created.kind),
        };

        let list_request = mock_permission_request_with_input(
            "perm-cron-list".to_string(),
            "toolu_cron_list".to_string(),
            "CronList".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let listed = LocalToolExecutor
            .result_after_permission(
                &list_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("CronList should produce a tool_result");
        match user_tool_result_block(&listed) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_cron_list");
                assert_eq!(
                    block.content,
                    format!(
                        "{created_id} — Every 5 minutes (recurring) [session-only]: check deploy"
                    )
                );
                let raw = block.tool_use_result.as_ref().expect("raw rides the row");
                assert_eq!(raw["jobs"][0]["id"], serde_json::json!(created_id));
                assert_eq!(
                    raw["jobs"][0]["humanSchedule"],
                    serde_json::json!("Every 5 minutes")
                );
            }
            other => panic!("unexpected CronList result: {other:?}"),
        }

        let delete_request = mock_permission_request_with_input(
            "perm-cron-delete".to_string(),
            "toolu_cron_delete".to_string(),
            "CronDelete".to_string(),
            created_id.clone(),
            serde_json::json!({"id": created_id.clone()}),
            PermissionMode::Default,
        );
        let deleted = LocalToolExecutor
            .result_after_permission(
                &delete_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("CronDelete should produce a tool_result");
        match user_tool_result_block(&deleted) {
            Some(block) => {
                assert_eq!(block.content, format!("Cancelled job {created_id}."));
                assert!(matches!(
                    block.tool_use_result.as_ref(),
                    Some(raw) if raw["id"] == serde_json::json!(created_id)
                ));
            }
            None => panic!("unexpected CronDelete result: {:?}", deleted.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_team_tools_return_official_json_payload_without_writes() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let config_root = std::env::temp_dir().join(format!(
            "cometix-executor-team-tools-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_root);
        let _team_io_guard = EnvVarGuard::unset("COMETIX_TEST_TEAM_FILE_IO");
        *TEAM_TOOL_STATE.lock().unwrap() = None;
        crate::utils::tasks::clear_leader_team_name();
        TASK_TOOL_STORE.lock().unwrap().clear();
        let create_request = mock_permission_request_with_input(
            "perm-team-create".to_string(),
            "toolu_team_create".to_string(),
            "TeamCreate".to_string(),
            "reviewers".to_string(),
            serde_json::json!({"team_name": "reviewers", "description": "Review code"}),
            PermissionMode::Default,
        );
        let created = LocalToolExecutor
            .result_after_permission(
                &create_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TeamCreate should produce a tool_result");
        match user_tool_result_block(&created) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_team_create");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                let payload: serde_json::Value = serde_json::from_str(&block.content).unwrap();
                assert_eq!(payload["team_name"], "reviewers");
                assert_eq!(payload["lead_agent_id"], "team-lead@reviewers");
                assert!(payload["team_file_path"].as_str().is_some_and(|path| {
                    path.ends_with(".claude/teams/reviewers/config.json")
                        || path.ends_with("teams/reviewers/config.json")
                }));
                // CC TeamCreateTool defines no renderToolResultMessage
                // (UI.tsx exports only renderToolUseMessage) — hidden at
                // render time by name.
            }
            None => panic!("unexpected TeamCreate result: {:?}", created.kind),
        }

        let delete_request = mock_permission_request_with_input(
            "perm-team-delete".to_string(),
            "toolu_team_delete".to_string(),
            "TeamDelete".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let deleted = LocalToolExecutor
            .result_after_permission(
                &delete_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TeamDelete should produce a tool_result");
        match user_tool_result_block(&deleted) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_team_delete");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                let payload: serde_json::Value = serde_json::from_str(&block.content).unwrap();
                assert_eq!(payload["success"], true);
                assert_eq!(payload["team_name"], "reviewers");
                assert!(
                    payload["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("reviewers"))
                );
                // CC TeamDeleteTool/UI.tsx:11-25 renderToolResultMessage
                // always returns null — hidden at render time by name.
            }
            other => panic!("unexpected TeamDelete result: {other:?}"),
        }

        *TEAM_TOOL_STATE.lock().unwrap() = None;
        crate::utils::tasks::clear_leader_team_name();
        TASK_TOOL_STORE.lock().unwrap().clear();
        let _ = std::fs::remove_dir_all(config_root);
    }

    #[tokio::test]
    async fn local_tool_executor_config_get_returns_official_model_result_without_writes() {
        let request = mock_permission_request_with_input(
            "perm-config".to_string(),
            "toolu_config".to_string(),
            "Config".to_string(),
            "theme".to_string(),
            serde_json::json!({"setting": "theme"}),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("Config get should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_config");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(block.content.starts_with("theme = "));
                let model_message = transcript_tool_result_to_model_message(
                    &RenderableMessage::user_tool_result(
                        "config-result",
                        block.tool_use_id.0.clone(),
                        block.content.clone(),
                        block.is_error,
                    ),
                    "Config",
                )
                .expect("Config mapped content should be forwarded as model content");
                assert!(matches!(
                    model_message.content.first(),
                    Some(UserContent::ToolResult(result)) if result.content == block.content
                ));
                // No display shape — the raw Output object rides the row.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("raw output should ride the row");
                assert_eq!(raw.get("success"), Some(&serde_json::json!(true)));
                assert_eq!(raw.get("operation"), Some(&serde_json::json!("get")));
                assert_eq!(raw.get("setting"), Some(&serde_json::json!("theme")));
            }
            None => panic!("unexpected Config result: {:?}", message.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_send_message_returns_official_json_payload_and_display_shape() {
        let request = mock_permission_request_with_input(
            "perm-send-message".to_string(),
            "toolu_send_message".to_string(),
            "SendMessage".to_string(),
            "to alice".to_string(),
            serde_json::json!({"to": "alice", "summary": "greet", "message": "hello"}),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("SendMessage should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_send_message");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                let payload: serde_json::Value = serde_json::from_str(&block.content).unwrap();
                assert_eq!(payload["success"], serde_json::json!(true));
                assert_eq!(
                    payload["message"],
                    serde_json::json!("Message sent to alice's inbox")
                );
                assert!(payload.get("routing").is_some());
                // CC SendMessageTool/UI.tsx:27-29: a routed result renders
                // null (`if ('routing' in result && result.routing) return
                // null`) — a render-time decision keyed on name + raw
                // (success_tool_result_is_nonvisual "sendmessage").
            }
            None => panic!("unexpected SendMessage result: {:?}", message.kind),
        }
        assert!(
            !crate::components::messages::user_tool_result_message::transcript_tool_result_should_emit_ui(
                &message,
                Some("SendMessage"),
            ),
            "routed SendMessage success rows must be dropped by the emit-UI gate"
        );
    }

    #[test]
    fn send_message_plain_result_keeps_visible_display_like_official() {
        // CC SendMessageTool/UI.tsx:35-38: without routing or a
        // request/target pair the result renders `result.message` — the live
        // seam keeps the row visible (Generic; the raw drives the
        // renderer) and the trait projects the official payload.
        use crate::tool::ToolCall as _;
        let output = crate::tool::ToolOutput::SendMessage(
            crate::tools::send_message_tool::SendMessageOutput {
                success: true,
                message: "Message delivered".to_string(),
                recipients: Vec::new(),
                request_id: None,
                target: None,
                routing: None,
            },
        );
        let raw = crate::tools::send_message_tool::SendMessageTool
            .tool_use_result(&output)
            .expect("raw payload should ride the row");
        assert_eq!(
            raw.get("message"),
            Some(&serde_json::json!("Message delivered"))
        );
    }

    #[tokio::test]
    async fn local_tool_executor_lsp_returns_safe_not_initialized_result() {
        let request = mock_permission_request_with_input(
            "perm-lsp".to_string(),
            "toolu_lsp".to_string(),
            "LSP".to_string(),
            "findReferences src/main.rs".to_string(),
            serde_json::json!({
                "operation": "findReferences",
                "filePath": "src/main.rs",
                "line": 1,
                "character": 1
            }),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("LSP should produce a safe tool_result");

        // No display shape — the raw Output object rides the row.
        let block = user_tool_result_block(&message).expect("LSP should produce a tool_result");
        assert_eq!(block.tool_use_id.0, "toolu_lsp");
        assert!(!block.is_error);
        assert_eq!(
            block.content,
            "LSP server manager not initialized. This may indicate a startup issue."
        );
        let raw = block
            .tool_use_result
            .as_ref()
            .expect("raw output should ride the row");
        assert_eq!(
            raw.get("operation"),
            Some(&serde_json::json!("findReferences"))
        );
        assert_eq!(raw.get("result"), Some(&serde_json::json!(block.content)));
        assert_eq!(raw.get("resultCount"), None);
        assert_eq!(raw.get("fileCount"), None);
    }

    #[tokio::test]
    async fn local_tool_executor_brief_displays_user_message() {
        let request = mock_permission_request_with_input(
            "perm-brief".to_string(),
            "toolu_brief".to_string(),
            "SendUserMessage".to_string(),
            "hello".to_string(),
            serde_json::json!({"message": "Hello **there**", "status": "normal"}),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("SendUserMessage should produce a tool_result");

        // No display shape — the raw Output object rides the row.
        let block = user_tool_result_block(&message).expect("Brief should produce a tool_result");
        assert_eq!(block.tool_use_id.0, "toolu_brief");
        assert!(!block.is_error);
        assert_eq!(block.content, "Message delivered to user.");
        let raw = block
            .tool_use_result
            .as_ref()
            .expect("raw output should ride the row");
        assert_eq!(
            raw.get("message"),
            Some(&serde_json::json!("Hello **there**"))
        );
        assert!(raw.get("attachments").is_none());
    }

    #[tokio::test]
    async fn local_tool_executor_ask_user_question_returns_official_output_shape() {
        let request = mock_permission_request_with_input(
            "perm-ask".to_string(),
            "toolu_ask".to_string(),
            "AskUserQuestion".to_string(),
            "Answer questions?".to_string(),
            serde_json::json!({
                "questions": [{
                    "question": "Proceed?",
                    "header": "Proceed",
                    "options": [
                        {"label": "Yes", "description": "Continue"},
                        {"label": "No", "description": "Stop"}
                    ]
                }],
                "answers": {"Proceed?": "Yes"},
                "annotations": {"Proceed?": {"notes": "Looks good"}}
            }),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("AskUserQuestion should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_ask");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(block.content.contains("User has answered your questions"));
                assert!(block.content.contains("\"Proceed?\"=\"Yes\""));
                assert!(block.content.contains("user notes: Looks good"));
                let model_message = transcript_tool_result_to_model_message(
                    &RenderableMessage::user_tool_result(
                        "ask-result",
                        block.tool_use_id.0.clone(),
                        block.content.clone(),
                        block.is_error,
                    ),
                    "AskUserQuestion",
                )
                .expect("AskUserQuestion official mapped content should remain model content");
                assert!(matches!(
                    model_message.content.first(),
                    Some(UserContent::ToolResult(result)) if result.content == block.content
                ));
                // No display shape — the raw Output object rides the row.
                let raw = block
                    .tool_use_result
                    .as_ref()
                    .expect("raw output should ride the row");
                assert_eq!(
                    raw.get("answers"),
                    Some(&serde_json::json!({"Proceed?": "Yes"}))
                );
            }
            other => panic!("unexpected AskUserQuestion result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_web_tools_return_official_output_shape_without_network() {
        let fetch_request = mock_permission_request_with_input(
            "perm-web-fetch".to_string(),
            "toolu_web_fetch".to_string(),
            "WebFetch".to_string(),
            "input:not a url".to_string(),
            serde_json::json!({"url": "not a url", "prompt": "summarize"}),
            PermissionMode::Default,
        );
        let fetch = LocalToolExecutor
            .result_after_permission(
                &fetch_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("WebFetch should produce a deterministic tool_result");
        match user_tool_result_block(&fetch) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_web_fetch");
                assert_eq!(block.derived_status(), ToolResultStatus::Error);
                assert!(block.content.contains("Invalid URL"));
                let model_message = transcript_tool_result_to_model_message(
                    &RenderableMessage::user_tool_result(
                        "webfetch-result",
                        block.tool_use_id.0.clone(),
                        block.content.clone(),
                        block.is_error,
                    ),
                    "WebFetch",
                )
                .expect("WebFetch mapped content should be forwarded as model content");
                assert!(matches!(
                    model_message.content.first(),
                    Some(UserContent::ToolResult(result)) if result.content == block.content && result.is_error
                ));
                // The error rides the row as the bare `Error: …` raw
                // string.
                assert!(matches!(
                    block.tool_use_result.as_ref(),
                    Some(serde_json::Value::String(raw)) if raw.contains("Invalid URL")
                ));
            }
            other => panic!("unexpected WebFetch result: {other:?}"),
        }

        let search_request = mock_permission_request_with_input(
            "perm-web-search".to_string(),
            "toolu_web_search".to_string(),
            "WebSearch".to_string(),
            "".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let search = LocalToolExecutor
            .result_after_permission(
                &search_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("WebSearch validation should produce a tool_result without network");
        match user_tool_result_block(&search) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_web_search");
                assert_eq!(block.derived_status(), ToolResultStatus::Error);
                assert!(block.content.contains("Error: Missing query"));
                let model_message = transcript_tool_result_to_model_message(
                    &RenderableMessage::user_tool_result(
                        "websearch-result",
                        block.tool_use_id.0.clone(),
                        block.content.clone(),
                        block.is_error,
                    ),
                    "WebSearch",
                )
                .expect("WebSearch error content should be forwarded as model content");
                assert!(matches!(
                    model_message.content.first(),
                    Some(UserContent::ToolResult(result)) if result.content == block.content && result.is_error
                ));
                // The error rides the row as the bare `Error: …` raw
                // string.
                assert!(matches!(
                    block.tool_use_result.as_ref(),
                    Some(serde_json::Value::String(raw)) if raw.contains("Error: Missing query")
                ));
            }
            None => panic!("unexpected WebSearch result: {:?}", search.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_worktree_tools_create_and_exit_session() {
        // Maps to real EnterWorktree/ExitWorktree call path (no longer safe no-op).
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let cwd_state_guard = CwdStateGuard::capture();
        crate::utils::worktree::restore_worktree_session(None);

        let repo =
            std::env::temp_dir().join(format!("cometix-worktree-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let status = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .expect("git init");
        assert!(status.success());
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo)
            .status();
        std::fs::write(repo.join("README"), "hi").expect("write");
        let _ = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status();

        let original_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&repo).expect("chdir repo");

        let enter_request = mock_permission_request_with_input(
            "perm-enter-worktree".to_string(),
            "toolu_enter_worktree".to_string(),
            "EnterWorktree".to_string(),
            "feature-demo".to_string(),
            serde_json::json!({"name": "feature-demo"}),
            PermissionMode::Default,
        );
        let enter = LocalToolExecutor
            .result_after_permission(
                &enter_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("EnterWorktree should produce a tool_result");
        match user_tool_result_block(&enter) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_enter_worktree");
                assert_eq!(
                    block.derived_status(),
                    ToolResultStatus::Success,
                    "content={}",
                    block.content
                );
                assert!(
                    block.content.contains("Created worktree"),
                    "content={}",
                    block.content
                );
                // No display shape — the raw rides the row.
                assert!(matches!(
                    block.tool_use_result.as_ref(),
                    Some(raw) if raw["worktreePath"]
                        .as_str()
                        .is_some_and(|path| path.contains("feature-demo"))
                ));
            }
            other => panic!("unexpected EnterWorktree result: {other:?}"),
        }
        assert!(crate::utils::worktree::get_current_worktree_session().is_some());

        let exit_request = mock_permission_request_with_input(
            "perm-exit-worktree".to_string(),
            "toolu_exit_worktree".to_string(),
            "ExitWorktree".to_string(),
            "keep".to_string(),
            serde_json::json!({"action": "keep"}),
            PermissionMode::Default,
        );
        let exit = LocalToolExecutor
            .result_after_permission(
                &exit_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("ExitWorktree should produce a tool_result");
        match user_tool_result_block(&exit) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_exit_worktree");
                assert_eq!(
                    block.derived_status(),
                    ToolResultStatus::Success,
                    "content={}",
                    block.content
                );
                assert!(
                    block.content.contains("Exited worktree")
                        || block.content.contains("preserved"),
                    "content={}",
                    block.content
                );
                // No display shape — the raw rides the row.
                assert!(matches!(
                    block.tool_use_result.as_ref(),
                    Some(raw) if raw["action"] == serde_json::json!("keep")
                ));
            }
            other => panic!("unexpected ExitWorktree result: {other:?}"),
        }
        assert!(crate::utils::worktree::get_current_worktree_session().is_none());

        let _ = std::env::set_current_dir(original_cwd);
        drop(cwd_state_guard);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn local_tool_executor_mcp_resource_tools_return_safe_official_empty_or_error_results() {
        let list_request = mock_permission_request_with_input(
            "perm-mcp-list".to_string(),
            "toolu_mcp_list".to_string(),
            "ListMcpResourcesTool".to_string(),
            "{}".to_string(),
            serde_json::json!({}),
            PermissionMode::Default,
        );
        let list_message = LocalToolExecutor
            .result_after_permission(
                &list_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("ListMcpResourcesTool should produce a tool_result");
        match user_tool_result_block(&list_message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_mcp_list");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert_eq!(
                    block.content,
                    "No resources found. MCP servers may still provide tools even if they have no resources."
                );
                // No display shape — the raw (an empty array) rides the
                // row and the by-tool-name renderer derives "(No resources
                // found)" from it.
                assert_eq!(block.tool_use_result.as_ref(), Some(&serde_json::json!([])));
            }
            other => panic!("unexpected ListMcpResourcesTool result: {other:?}"),
        }

        let read_request = mock_permission_request_with_input(
            "perm-mcp-read".to_string(),
            "toolu_mcp_read".to_string(),
            "ReadMcpResourceTool".to_string(),
            "memory mem://note".to_string(),
            serde_json::json!({"server": "memory", "uri": "mem://note"}),
            PermissionMode::Default,
        );
        let read_message = LocalToolExecutor
            .result_after_permission(
                &read_request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("ReadMcpResourceTool should produce a safe error tool_result");
        assert!(matches!(
            user_tool_result_block(&read_message),
            Some(block) if block.tool_use_id.0 == "toolu_mcp_read"
                && block.derived_status() == ToolResultStatus::Error
                && block.content == "Server \"memory\" not found. Available servers: "
        ));
    }

    #[tokio::test]
    async fn local_tool_executor_todowrite_returns_official_model_result_copy() {
        let request = mock_permission_request_with_input(
            "perm-todos".to_string(),
            "toolu_todos".to_string(),
            "TodoWrite".to_string(),
            "2 todos".to_string(),
            serde_json::json!({
                "todos": [
                    {"content": "Inspect state", "status": "completed", "activeForm": "Inspecting state"},
                    {"content": "Run tests", "status": "pending", "activeForm": "Running tests"}
                ]
            }),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("TodoWrite should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.tool_use_id.0, "toolu_todos");
                assert_eq!(block.derived_status(), ToolResultStatus::Success);
                assert!(
                    block
                        .content
                        .contains("Todos have been modified successfully")
                );
                assert!(block.content.contains("continue to use the todo list"));
                // CC TodoWriteTool.ts defines no renderToolResultMessage
                // (userFacingName '' :48-50, renderToolUseMessage → null
                // :62-64) — a render-time decision keyed on the name; the
                // visible todo UI arrives through the todo attachment instead.
            }
            None => panic!("unexpected TodoWrite result: {:?}", message.kind),
        }
        assert!(
            !crate::components::messages::user_tool_result_message::transcript_tool_result_should_emit_ui(
                &message,
                Some("TodoWrite"),
            ),
            "TodoWrite success rows must be dropped by the emit-UI gate"
        );
    }

    #[test]
    fn edit_no_write_preflight_stops_before_permission_and_hook_orchestration() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-edit-disabled".to_string()),
            name: "Edit".to_string(),
            input: serde_json::json!({
                "file_path": "/tmp/disabled.txt",
                "old_string": "",
                "new_string": "blocked"
            }),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant, &ToolUseContext::default(), &mut queue);
        assert!(update.request.is_none());
        assert!(queue.is_empty());
        assert!(matches!(
            update.message.as_ref().and_then(user_tool_result_block),
            Some(block) if block.derived_status() == ToolResultStatus::Error
                && block.content == crate::tools::shared::write_gate::FILE_EDIT_DISABLED_ERROR
        ));
    }

    #[test]
    fn edit_semantic_validation_uses_official_error_envelope_tool_use_result_and_ui() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-semantic-validation-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("value.txt");
        std::fs::write(&path, "old\n").unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-edit-unread".to_string()),
            name: "Edit".to_string(),
            input: serde_json::json!({
                "file_path": path.display().to_string(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let context = ToolUseContext::default();
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant, &context, &mut queue);
        assert!(update.request.is_none());
        assert!(queue.is_empty());
        let message = update.message.expect("semantic validation result");
        // CC's internal `Error: ...` `toolUseResult` string rides the row; no display.
        let block = user_tool_result_block(&message).expect("validation row");
        assert!(block.is_error);
        assert_eq!(
            block.content,
            "<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>"
        );
        assert_eq!(
            block.tool_use_result,
            Some(serde_json::Value::String(
                "Error: File has not been read yet. Read it first before writing to it."
                    .to_string()
            ))
        );
        let model = update.tool_result.expect("model validation result");
        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.is_error
                    && result.tool_use_result == Some(serde_json::Value::String(
                        "Error: File has not been read yet. Read it first before writing to it."
                            .to_string()
                    ))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn notebook_edit_no_write_preflight_skips_hooks_and_preserves_error_tool_use_result() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let request = mock_permission_request_with_input(
            "perm-notebook-no-write",
            "toolu-notebook-no-write",
            "NotebookEdit",
            "/tmp/demo.ipynb".to_string(),
            serde_json::json!({
                "notebook_path": "/tmp/demo.ipynb",
                "cell_id": "cell-a",
                "new_source": "new"
            }),
            PermissionMode::Default,
        );
        let context = ToolUseContext::default();
        let result = check_permissions_and_call_tool_with_response_async(
            &request,
            &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
            false,
            None,
            &context,
            None,
        )
        .await;
        assert!(result.hook_messages.is_empty());
        assert!(result.pre_tool_messages.is_empty());
        assert!(result.post_tool_messages.is_empty());
        // The error-as-data object rides the row as the raw
        // `toolUseResult`; the display is Generic.
        assert!(matches!(
            result.message.as_ref().and_then(user_tool_result_block),
            Some(crate::types::message::ToolResult {
                is_error: true,
                content,                tool_use_result: Some(raw),
                ..
            }) if content == crate::tools::shared::write_gate::NOTEBOOK_EDIT_DISABLED_ERROR
                && raw["error"] == serde_json::json!(content)
        ));
        assert!(matches!(
            result
                .tool_result
                .as_ref()
                .and_then(|message| message.content.first()),
            Some(UserContent::ToolResult(block))
                if block.tool_use_result.as_ref().is_some_and(|raw| {
                    raw["error"]
                        == crate::tools::shared::write_gate::NOTEBOOK_EDIT_DISABLED_ERROR
                })
        ));
    }

    #[test]
    fn notebook_edit_semantic_validation_uses_error_tool_use_result_and_compact_ui() {
        let _guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!(
            "cometix-notebook-semantic-validation-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("demo.ipynb");
        std::fs::write(
            &path,
            serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [{"cell_type": "code", "id": "cell-a", "source": "old"}]
            })
            .to_string(),
        )
        .unwrap();
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu-notebook-unread".to_string()),
            name: "NotebookEdit".to_string(),
            input: serde_json::json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "new"
            }),
        };
        let assistant = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };
        let mut queue = Vec::new();
        let update = run_tool_use(&block, &assistant, &ToolUseContext::default(), &mut queue);
        assert!(update.request.is_none());
        let message = update.message.expect("Notebook validation result");
        // The raw `Error: …` string rides the row, not a display variant.
        let result_block = match user_tool_result_block(&message) {
            Some(
                result_block @ crate::types::message::ToolResult {
                    is_error: true,
                    tool_use_result: Some(serde_json::Value::String(raw)),
                    ..
                },
            ) => {
                assert_eq!(
                    result_block.content,
                    "<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>"
                );
                assert_eq!(
                    raw,
                    "Error: File has not been read yet. Read it first before writing to it."
                );
                result_block.clone()
            }
            other => panic!("unexpected Notebook validation result: {other:?}"),
        };
        let model = update.tool_result.expect("model validation result");
        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result))
                if result.is_error
                    && result.tool_use_result == Some(serde_json::Value::String(
                        "Error: File has not been read yet. Read it first before writing to it."
                            .to_string()
                    ))
        ));
        let lines = crate::components::messages::user_tool_result_message::render_tool_result_lines(
            "NotebookEdit",
            ToolResultStatus::Error,
            &result_block.content,
        );
        assert_eq!(lines[0].text, "Error editing notebook");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn edit_permission_rejection_records_bounded_diff_display_immediately() {
        let root = std::env::temp_dir().join(format!(
            "cometix-edit-live-rejection-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("value.txt");
        std::fs::write(&path, "before\nold\nafter\n").unwrap();
        let request = mock_permission_request_with_input(
            "perm-edit-rejected",
            "toolu-edit-rejected",
            "Edit",
            path.display().to_string(),
            serde_json::json!({
                "file_path": path.display().to_string(),
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
            PermissionMode::Default,
        );
        let message = permission_terminal_result(&request, ToolResultStatus::Rejected, false);
        let model = transcript_tool_result_to_model_message(&message, &request.tool_name)
            .expect("model rejection");
        // Every denied tool records `Error: {message}` as its raw
        // `toolUseResult` (CC toolExecution.ts:1068); the rejected preview is
        // still computed from the tool INPUT at render time
        // (`official_file_result_element`).
        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result)) if matches!(
                result.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw)) if raw.starts_with("Error: ")
            )
        ));
        assert!(matches!(
            user_tool_result_block(&message),
            Some(block) if matches!(
                block.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw)) if raw.starts_with("Error: ")
            )
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn notebook_edit_permission_rejection_records_error_raw_like_official() {
        let request = mock_permission_request_with_input(
            "perm-notebook-rejected",
            "toolu-notebook-rejected",
            "NotebookEdit",
            "/tmp/demo.ipynb".to_string(),
            serde_json::json!({
                "notebook_path": "/tmp/demo.ipynb",
                "cell_id": "cell-a",
                "new_source": "print('rejected')",
                "cell_type": "code",
                "edit_mode": "replace"
            }),
            PermissionMode::Default,
        );
        let message = permission_terminal_result(&request, ToolResultStatus::Rejected, false);
        let model = transcript_tool_result_to_model_message(&message, &request.tool_name)
            .expect("model rejection");
        // Every denied tool records `Error: {message}` as its raw
        // `toolUseResult` (CC toolExecution.ts:1068); the rejected leaf still
        // renders from the tool INPUT via `UserToolRejectMessage`.
        assert!(matches!(
            model.content.first(),
            Some(UserContent::ToolResult(result)) if matches!(
                result.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw)) if raw.starts_with("Error: ")
            )
        ));
        assert!(matches!(
            user_tool_result_block(&message),
            Some(block) if matches!(
                block.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw)) if raw.starts_with("Error: ")
            )
        ));
    }

    #[test]
    fn rejected_empty_old_string_uses_official_write_content_preview_shape() {
        let request = mock_permission_request_with_input(
            "perm-edit-create-rejected",
            "toolu-edit-create-rejected",
            "Edit",
            "/tmp/new.txt".to_string(),
            serde_json::json!({
                "file_path": "/tmp/new.txt",
                "old_string": "",
                "new_string": "one\ntwo",
                "replace_all": false
            }),
            PermissionMode::Default,
        );
        let message = permission_terminal_result(&request, ToolResultStatus::Rejected, false);
        // The new-file content preview is computed from the tool INPUT at
        // render time (FileEditTool/UI.tsx:149-159); the denied row records
        // `Error: {message}` as its raw (CC toolExecution.ts:1068).
        assert!(matches!(
            user_tool_result_block(&message),
            Some(block) if matches!(
                block.tool_use_result.as_ref(),
                Some(serde_json::Value::String(raw)) if raw.starts_with("Error: ")
            )
        ));
    }

    #[tokio::test]
    async fn local_tool_executor_permission_terminal_results_match_official_control_copy() {
        let request = mock_permission_request(
            "perm-bash".to_string(),
            "toolu_bash".to_string(),
            "Bash".to_string(),
            "echo fallback".to_string(),
            PermissionMode::Default,
        );

        let rejected = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::Deny,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("deny should produce a tool_result");
        // CC's ONLY cancelled tool_result producer is the pre-canUseTool abort
        // gate (`toolExecution.ts:415-453`) — cancellation is not a permission
        // answer. `PermissionPromptChoice::Cancel` used to drive this arm.
        let aborted_context = crate::tool::ToolUseContext::default();
        aborted_context.abort_controller.abort();
        let canceled = check_permissions_and_call_tool_with_response_async(
            &request,
            &PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce),
            false,
            None,
            &aborted_context,
            None,
        )
        .await
        .message
        .expect("an aborted turn should produce the cancelled tool_result");
        let rejected_with_feedback = LocalToolExecutor
            .check_permissions_and_call_tool(
                &request,
                &PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                    .with_feedback("  use a safer command  "),
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .message
            .expect("deny with feedback should produce a tool_result");

        assert!(matches!(
            user_tool_result_block(&rejected),
            Some(block) if block.derived_status() == ToolResultStatus::Rejected
                && block.content.contains("tool use was rejected")
        ));
        assert!(matches!(
            user_tool_result_block(&canceled),
            Some(block) if block.derived_status() == ToolResultStatus::Canceled
                && block.content.contains("doesn't want to take this action right now")
        ));
        // CC dispatches reject-with-reason through the error branch
        // (UserToolResultMessage.tsx:57 `startsWith(REJECT_MESSAGE)` is false
        // for the with-reason prefix; UserToolErrorMessage.tsx:63 handles it).
        assert!(matches!(
            user_tool_result_block(&rejected_with_feedback),
            Some(block) if block.is_error
                && block.content.starts_with(crate::utils::messages::REJECT_MESSAGE_WITH_REASON_PREFIX)
                && block.content.ends_with("use a safer command")
        ));
    }

    /// Maps to: CC `AskUserQuestionTool.tsx:285-292` — "User declined to
    /// answer questions" is `renderToolUseRejectedMessage`, a UI-only string.
    /// The MODEL receives the generic reject copy like every other tool
    /// (`PermissionContext.cancelAndAbort` → REJECT_MESSAGE family). Writing
    /// the UI string into the model result also broke `derived_status()`,
    /// whose Rejected arm keys on the sentinel.
    #[test]
    fn ask_user_question_rejected_model_result_is_the_generic_reject_copy() {
        let request = mock_permission_request(
            "perm-askq".to_string(),
            "toolu_askq".to_string(),
            "AskUserQuestion".to_string(),
            "questions".to_string(),
            PermissionMode::Default,
        );
        let rejected = permission_terminal_result_for_decision(
            &request,
            ToolResultStatus::Rejected,
            None,
            None,
            false,
        );
        let block = user_tool_result_block(&rejected).expect("rejected tool_result block");
        assert!(
            !block.content.contains("User declined to answer questions"),
            "the UI renderer owns that string; the model gets the reject copy"
        );
        assert!(
            block
                .content
                .starts_with(crate::utils::messages::REJECT_MESSAGE),
            "got: {}",
            block.content
        );
        assert_eq!(block.derived_status(), ToolResultStatus::Rejected);
    }

    /// CC `FallbackPermissionRequest.tsx:108-120` `handleCancel` (Esc) and
    /// `:99`/`PermissionRequest.tsx:210` (No / Ctrl-C) all call
    /// `toolUseConfirm.onReject()` → `cancelAndAbort`. A dialog answer carries
    /// no `decision_message`, so `tool_execution.rs:8298` aborts the main-loop
    /// controller (`PermissionContext.ts:166-171`).
    ///
    /// This used to be `PermissionPromptChoice::Cancel`, which skipped
    /// `cancel_and_abort` and left the turn running. The dialog now emits Deny.
    #[tokio::test]
    async fn dialog_reject_without_decision_message_aborts_the_main_loop() {
        let request = mock_permission_request(
            "perm-bash".to_string(),
            "toolu_bash".to_string(),
            "Bash".to_string(),
            "echo fallback".to_string(),
            PermissionMode::Default,
        );
        let context = crate::tool::ToolUseContext::default();
        assert!(!context.abort_controller.is_aborted());

        let _ = check_permissions_and_call_tool_with_response(
            &request,
            &PermissionPromptResponse::new(PermissionPromptChoice::Deny),
            &context,
            None,
        );

        assert!(
            context.abort_controller.is_aborted(),
            "CC's dialog Esc/No abort the main-loop controller"
        );
    }

    /// The same Deny, but WITH a `decision_message`, is a SYSTEM decision
    /// (`toolExecution.ts:1023`) — no dialog ran, nothing aborts.
    #[tokio::test]
    async fn system_deny_with_decision_message_does_not_abort() {
        let mut request = mock_permission_request(
            "perm-classifier".to_string(),
            "toolu_classifier".to_string(),
            "Bash".to_string(),
            "npm publish".to_string(),
            PermissionMode::Auto,
        );
        request.message = crate::utils::messages::build_yolo_rejection_message("blocked");
        let context = crate::tool::ToolUseContext::default();

        let _ = check_permissions_and_call_tool_with_response(
            &request,
            &PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                .with_decision_message(request.message.clone()),
            &context,
            None,
        );

        assert!(
            !context.abort_controller.is_aborted(),
            "a system deny must not abort the turn"
        );
    }

    /// Maps to: CC `services/tools/toolExecution.ts:1023-1068` —
    /// `let errorMessage = permissionDecision.message`, wrapped as the error
    /// `tool_result` content and as `toolUseResult: `Error: ${errorMessage}``.
    ///
    /// The auto-mode classifier's deny message is
    /// `buildYoloRejectionMessage(reason)` (`utils/permissions/permissions.ts:910`),
    /// so THAT copy is what reaches the model — not the generic REJECT_MESSAGE
    /// the dialog path produces. `PermissionContext.ts:154-172` is the dialog
    /// half: `cancelAndAbort` builds a fresh decision carrying REJECT_MESSAGE,
    /// which is why a dialog answer supplies no `decision_message` here.
    #[tokio::test]
    async fn permission_deny_result_matches_official_decision_message_for_the_model() {
        let mut request = mock_permission_request(
            "perm-classifier".to_string(),
            "toolu_classifier".to_string(),
            "Bash".to_string(),
            "npm publish".to_string(),
            PermissionMode::Auto,
        );
        request.message =
            crate::utils::messages::build_yolo_rejection_message("publishing is not allowed");

        let denied = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::Deny,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("deny should produce a tool_result");
        let block = user_tool_result_block(&denied).expect("tool_result block");
        assert_eq!(block.content, request.message);
        assert!(block.is_error);
        assert_eq!(
            block.tool_use_result,
            Some(serde_json::Value::String(format!(
                "Error: {}",
                request.message
            )))
        );
        // `isClassifierDenial` (`utils/messages.ts:259-261`) is what the
        // transcript uses to render the short summary for this row.
        assert!(crate::utils::messages::is_classifier_denial(&block.content));

        // The dialog answer keeps CC's `cancelAndAbort` copy: no decision
        // message is carried, so REJECT_MESSAGE stands even though the request
        // still holds the ask decision's own message.
        let dialog_rejected = LocalToolExecutor
            .check_permissions_and_call_tool(
                &request,
                &PermissionPromptResponse::new(PermissionPromptChoice::Deny),
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .message
            .expect("dialog deny should produce a tool_result");
        let dialog_block = user_tool_result_block(&dialog_rejected).expect("tool_result block");
        assert_eq!(dialog_block.content, crate::utils::messages::REJECT_MESSAGE);
    }

    #[tokio::test]
    async fn local_tool_executor_unknown_tool_returns_official_no_such_tool_error() {
        let request = mock_permission_request(
            "perm-unknown".to_string(),
            "toolu_unknown".to_string(),
            "mcp__nonexistent__tool".to_string(),
            "anything".to_string(),
            PermissionMode::Default,
        );
        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("unknown tool should produce an error tool_result");
        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.derived_status(), ToolResultStatus::Error);
                assert_eq!(
                    block.content,
                    "<tool_use_error>Error: No such tool available: mcp__nonexistent__tool</tool_use_error>"
                );
            }
            None => panic!("unexpected unknown-tool result: {:?}", message.kind),
        }
    }

    #[tokio::test]
    async fn local_tool_executor_agent_teammate_path_uses_shared_spawn_boundary() {
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-agent".to_string(),
            "toolu_agent".to_string(),
            "Agent".to_string(),
            "summarize workspace".to_string(),
            serde_json::json!({
                "description": "summarize",
                "prompt": "summarize workspace",
                "name": "worker",
                "team_name": "alpha"
            }),
            PermissionMode::Default,
        );

        let message = LocalToolExecutor
            .result_after_permission(
                &request,
                PermissionPromptChoice::AllowOnce,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
            )
            .await
            .expect("fallback should produce a tool_result");

        match user_tool_result_block(&message) {
            Some(block) => {
                assert_eq!(block.derived_status(), ToolResultStatus::Error);
                assert!(
                    block.content.contains("Team \"alpha\" does not exist")
                        || block
                            .content
                            .contains("Agent Teams is not yet available on your plan"),
                    "unexpected content: {}",
                    block.content
                );
            }
            None => panic!("unexpected fallback result: {:?}", message.kind),
        }
    }
}

#[cfg(test)]
mod permission_request_hook_decision_tests {
    //! CC PermissionContext.ts:230-262 → toolExecution.ts:1023-1068 regressions.
    use super::*;
    use crate::services::hooks::{HookCallback, RegisteredHook, RegisteredHookMatcher};
    use crate::types::permissions::{PermissionDecisionReason, PermissionMode};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn request() -> PermissionRequest {
        let mut request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "permission-hook-decision",
                "toolu_hook_decision",
                "Read",
                "/tmp/denied",
                json!({"file_path": "/tmp/denied"}),
                PermissionMode::Default,
            );
        request.message = "original permission question".to_string();
        request
    }

    // The loser starts first but waits for the winner, so these assertions check
    // completion order rather than relying on configuration order or wall time.
    fn competing_decisions(winner: Value, loser: Value) -> RegisteredHooks {
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let losing = RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let receiver = receiver.clone();
                let loser = loser.clone();
                Box::pin(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    json!({"hookSpecificOutput": {
                        "hookEventName": "PermissionRequest", "decision": loser
                    }})
                })
            }),
            timeout: Some(3),
        });
        let winning = RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let sender = sender.clone();
                let winner = winner.clone();
                Box::pin(async move {
                    sender.send(()).await.unwrap();
                    json!({"hookSpecificOutput": {
                        "hookEventName": "PermissionRequest", "decision": winner
                    }})
                })
            }),
            timeout: Some(3),
        });
        std::collections::HashMap::from([(
            "PermissionRequest".into(),
            vec![RegisteredHookMatcher {
                matcher: Some("Read".into()),
                hooks: vec![losing, winning],
                ..Default::default()
            }],
        )])
    }

    #[tokio::test]
    async fn permission_request_hook_deny_message_reason_and_terminal_result_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        for reason in [Some("policy blocks this read"), Some(""), None] {
            let mut decision = json!({"behavior": "deny"});
            if let Some(reason) = reason {
                decision["message"] = json!(reason);
            }
            let config = competing_decisions(
                decision,
                json!({
                    "behavior": "allow", "updatedInput": {"file_path": "/tmp/loser"},
                    "updatedPermissions": [{"type": "addRules", "destination": "session",
                        "behavior": "allow", "rules": [{"toolName": "Read"}]}]
                }),
            );
            let mut context = ToolUseContext::default();
            context.tools = vec![crate::tools::file_read_tool::file_read_tool_schema()];
            let original = request();
            let prepared = prepare_permission_prompt_hooks_with_config(
                &original,
                &context,
                Some(&config),
                Vec::new(),
                Some(&context.abort_controller),
            )
            .await;
            let expected = reason
                .filter(|value| !value.is_empty())
                .unwrap_or("Permission denied by hook");
            assert_eq!(prepared.forced_choice, Some(PermissionPromptChoice::Deny));
            assert_eq!(prepared.request.message, expected);
            assert_eq!(prepared.request.input, original.input);
            assert!(
                prepared.permission_updates.is_empty(),
                "the losing allow cannot grant rules"
            );
            assert_eq!(
                prepared.request.decision_reason,
                Some(PermissionDecisionReason::Hook {
                    hook_name: "PermissionRequest".into(),
                    hook_source: None,
                    reason: reason.map(str::to_string),
                })
            );
            let required = apply_required_can_use_tool_after_hooks(
                prepared.request,
                &context,
                None,
                prepared.forced_choice,
                false,
            )
            .await
            .expect("permission check should not abort");
            assert_eq!(required.request.message, expected);
            let result = LocalToolExecutor
                .check_permissions_and_call_tool(
                    &required.request,
                    &PermissionPromptResponse::new(required.forced_choice.unwrap())
                        .with_decision_message(required.request.message.clone()),
                    &context,
                    None,
                    None,
                )
                .await;
            let message = result
                .message
                .expect("resolved deny produces a model result");
            let block = user_tool_result_block(&message).expect("tool result");
            assert_eq!(block.content, expected);
            assert!(block.is_error);
            assert_eq!(
                block.tool_use_result,
                Some(json!(format!("Error: {expected}")))
            );
            assert!(
                !context.abort_controller.is_aborted(),
                "noninterrupt hook deny continues the turn"
            );

            // The retained dialog must still use cancelAndAbort, even when its
            // request happens to carry a prior hook/system decision message.
            let dialog = LocalToolExecutor
                .check_permissions_and_call_tool(
                    &required.request,
                    &PermissionPromptResponse::new(PermissionPromptChoice::Deny),
                    &context,
                    None,
                    None,
                )
                .await
                .message
                .expect("dialog reject result");
            assert_eq!(
                user_tool_result_block(&dialog).unwrap().content,
                crate::utils::messages::REJECT_MESSAGE
            );
            assert!(context.abort_controller.is_aborted());
        }
    }

    #[tokio::test]
    async fn permission_request_hook_winning_allow_isolation_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        let config = competing_decisions(
            json!({"behavior": "allow", "updatedInput": {"file_path": "/tmp/winner"}}),
            json!({"behavior": "deny", "message": "losing deny", "interrupt": true}),
        );
        let original = request();
        let context = ToolUseContext::default();
        let prepared = prepare_permission_prompt_hooks_with_config(
            &original,
            &context,
            Some(&config),
            Vec::new(),
            Some(&context.abort_controller),
        )
        .await;
        assert_eq!(
            prepared.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert!(prepared.request.message.is_empty());
        assert_eq!(
            prepared.request.decision_reason,
            Some(PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".into(),
                hook_source: None,
                reason: None,
            })
        );
        assert_eq!(prepared.request.input, json!({"file_path": "/tmp/winner"}));
        assert!(!context.abort_controller.is_aborted());
    }
}

#[cfg(test)]
mod pre_tool_hook_decision_tests {
    //! CC toolHooks.ts:481-557 and toolExecution.ts:831-837,1023-1068.
    use super::*;
    use crate::services::hooks::{HookCallback, RegisteredHook, RegisteredHookMatcher};
    use crate::types::permissions::PermissionDecisionReason;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn hook_output(behavior: &str, reason: Option<&str>) -> Value {
        let mut output = json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": behavior
        }});
        if let Some(reason) = reason {
            output["hookSpecificOutput"]["permissionDecisionReason"] = json!(reason);
        }
        output
    }

    fn ordered_config(first: Value, second: Option<Value>) -> RegisteredHooks {
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let signal_later = second.is_some();
        let first = RegisteredHook::Callback(HookCallback {
            callback: Arc::new(move |_, _| {
                let sender = sender.clone();
                let output = first.clone();
                Box::pin(async move {
                    if signal_later {
                        sender.send(()).await.unwrap();
                    }
                    output
                })
            }),
            timeout: Some(3),
        });
        let mut hooks = vec![first];
        if let Some(second) = second {
            // Put the later completion first in configuration to reject a
            // configuration-order implementation as well as a sticky reason.
            hooks.insert(
                0,
                RegisteredHook::Callback(HookCallback {
                    callback: Arc::new(move |_, _| {
                        let receiver = receiver.clone();
                        let output = second.clone();
                        Box::pin(async move {
                            tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                receiver.recv(),
                            )
                            .await
                            .unwrap()
                            .unwrap();
                            output
                        })
                    }),
                    timeout: Some(3),
                }),
            );
        }
        std::collections::HashMap::from([(
            "PreToolUse".into(),
            vec![RegisteredHookMatcher {
                matcher: Some("Read".into()),
                hooks,
                ..Default::default()
            }],
        )])
    }

    fn read_request() -> PermissionRequest {
        let mut request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "pre-hook",
                "toolu_pre_hook",
                "Read",
                "/tmp/pre-hook",
                json!({"file_path":"/tmp/pre-hook"}),
                PermissionMode::Default,
            );
        request.message.clear();
        request.decision_reason = None;
        request
    }

    #[tokio::test]
    async fn pre_tool_hook_prepare_message_and_reason_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        for behavior in ["allow", "ask", "deny"] {
            for reason in [Some("completion reason"), Some(""), None] {
                let config = ordered_config(hook_output(behavior, reason), None);
                let context = ToolUseContext::default();
                let prepared = prepare_permission_request_before_prompt_with_config(
                    &read_request(),
                    &context,
                    Some(&config),
                    Vec::new(),
                    Some(&context.abort_controller),
                )
                .await;
                let fallback = if behavior == "ask" {
                    "Hook PreToolUse:Read asked for confirmation for this tool"
                } else {
                    "Hook PreToolUse:Read denied this tool"
                };
                let expected = if behavior == "allow" {
                    ""
                } else {
                    reason
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or(fallback)
                };
                assert_eq!(prepared.request.message, expected);
                assert_eq!(
                    prepared.request.decision_reason,
                    Some(PermissionDecisionReason::Hook {
                        hook_name: "PreToolUse:Read".into(),
                        hook_source: Some("settings".into()),
                        reason: reason.map(str::to_string),
                    })
                );
                assert_eq!(prepared.force_ask, behavior == "ask");
                assert_eq!(
                    prepared.forced_choice,
                    match behavior {
                        "deny" => Some(PermissionPromptChoice::Deny),
                        "allow" => Some(PermissionPromptChoice::AllowOnce),
                        _ => None,
                    }
                );
                assert!(!context.abort_controller.is_aborted());
            }
        }
    }

    #[tokio::test]
    async fn pre_tool_hook_aggregated_deny_configured_terminal_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _managed = crate::services::hooks::test_support::ManagedSettingsGuard::install(None);
        let _simple = EnvVarGuard::unset("CLAUDE_CODE_SIMPLE");
        for (later, expected_reason, expected_message) in [
            (
                hook_output("allow", Some("later allow reason")),
                Some("later allow reason"),
                "later allow reason",
            ),
            (
                hook_output("allow", Some("")),
                Some(""),
                "Hook PreToolUse:Read denied this tool",
            ),
            (
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                "updatedInput": {"file_path": "/tmp/unused"}}}),
                None,
                "Hook PreToolUse:Read denied this tool",
            ),
        ] {
            let config = ordered_config(hook_output("deny", Some("first deny")), Some(later));
            let context = ToolUseContext::default();
            let prepared = prepare_permission_request_before_prompt_with_config(
                &read_request(),
                &context,
                Some(&config),
                Vec::new(),
                Some(&context.abort_controller),
            )
            .await;
            assert_eq!(prepared.request.message, expected_message);
            assert_eq!(prepared.request.input, read_request().input);
            assert_eq!(
                prepared.request.decision_reason,
                Some(PermissionDecisionReason::Hook {
                    hook_name: "PreToolUse:Read".into(),
                    hook_source: Some("settings".into()),
                    reason: expected_reason.map(str::to_string),
                })
            );
            // New callback futures/channels for a second real invocation.
            let later = if let Some(reason) = expected_reason {
                hook_output("allow", Some(reason))
            } else {
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": {"file_path": "/tmp/unused"}}})
            };
            let config = ordered_config(hook_output("deny", Some("first deny")), Some(later));
            let result = check_permissions_and_call_tool_with_config(
                &read_request(),
                PermissionPromptChoice::AllowOnce,
                &context,
                None,
                &config,
                Vec::new(),
            )
            .await;
            let message = result.message.expect("denied tool result");
            let block = user_tool_result_block(&message).unwrap();
            assert_eq!(block.content, expected_message);
            assert_eq!(
                block.tool_use_result,
                Some(json!(format!("Error: {expected_message}")))
            );
            assert!(block.is_error);
            assert!(
                !context.abort_controller.is_aborted(),
                "hook denial must not enter dialog cancelAndAbort"
            );
        }
    }
}

#[cfg(test)]
mod permission_request_mutation_tests {
    use super::*;
    use crate::services::hooks::{HookCallback, RegisteredHook, RegisteredHookMatcher};
    use crate::services::tools::streaming_tool_executor::streaming_hook_decision_tests::PermissionRequestFixture;
    use crate::tool::ToolPermissionContext;
    use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};
    use crate::utils::test_env::TEST_ENV_LOCK;
    use serde_json::json;
    use std::sync::Arc;

    fn allow_input(input: serde_json::Value) -> RegisteredHooks {
        std::collections::HashMap::from([(
            "PermissionRequest".into(),
            vec![RegisteredHookMatcher {
                matcher: Some("Edit".into()),
                hooks: vec![RegisteredHook::Callback(HookCallback {
                    callback: Arc::new(move |_, _| {
                        let input = input.clone();
                        Box::pin(async move {
                            json!({"hookSpecificOutput": {
                                "hookEventName":"PermissionRequest",
                                "decision":{"behavior":"allow", "updatedInput": input}
                            }})
                        })
                    }),
                    timeout: Some(3),
                })],
                ..Default::default()
            }],
        )])
    }

    fn record_read(context: &ToolUseContext, path: &std::path::Path) {
        context.read_file_state.set_entry(ReadFileStateEntry {
            path: path.display().to_string(),
            content: Some(std::fs::read_to_string(path).unwrap()),
            timestamp_ms: crate::utils::file::get_file_modification_time(path),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        });
    }

    fn edit_block(path: &std::path::Path) -> ToolUseBlock {
        ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_pr_edit_destination".into()),
            name: "Edit".into(),
            input: json!({"file_path": path,
                "old_string":"old", "new_string":"permission-approved", "replace_all":false}),
        }
    }

    fn assistant(block: &ToolUseBlock) -> AssistantMessage {
        AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                block.clone(),
            )],
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    /// CC PermissionContext.ts:233-239 / toolExecution.ts:1129-1133,1181-1209:
    /// permission.updatedInput replaces the approved logical call destination.
    #[tokio::test]
    async fn permission_request_hook_edit_updated_destination_matches_official() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let fixture = PermissionRequestFixture::new("1");
        crate::bootstrap::state::replace_registered_hooks(Default::default());
        let original = fixture.root.join("workspace/original.txt");
        std::fs::write(&original, "old\n").unwrap();
        std::fs::write(&fixture.target, "old\n").unwrap();
        let mut context = fixture.context();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];
        record_read(&context, &original);
        record_read(&context, &fixture.target);
        let block = edit_block(&original);
        let assistant = assistant(&block);
        let request = run_tool_use(&block, &assistant, &context, &mut Vec::new())
            .request
            .unwrap();
        let key = crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY;
        assert_eq!(request.call_input.as_ref().unwrap()[key], json!(original));
        let updated = edit_block(&fixture.target).input;
        let prepared = prepare_permission_prompt_hooks_with_config(
            &request,
            &context,
            Some(&allow_input(updated.clone())),
            Vec::new(),
            Some(&context.abort_controller),
        )
        .await;
        assert_eq!(
            prepared.forced_choice,
            Some(PermissionPromptChoice::AllowOnce)
        );
        assert_eq!(prepared.request.input, updated);
        assert_eq!(
            prepared.request.call_input.as_ref().unwrap()[key],
            json!(fixture.target)
        );
        // Same post-PreToolUse continuation the real streaming/query callers
        // use: response.updated_input is absent, so prepare must carry the pin.
        let response = PermissionPromptResponse::new(prepared.forced_choice.unwrap());
        assert!(response.updated_input.is_none());
        let result = streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
            &prepared.request,
            &response,
            false,
            None,
            &context,
            Some(&assistant),
        )
        .await;
        let message = result.message.expect("actual Edit result");
        let result = user_tool_result_block(&message).unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "old\n");
        assert_eq!(
            std::fs::read_to_string(&fixture.target).unwrap(),
            "permission-approved\n"
        );
        assert!(!context.abort_controller.is_aborted());
    }

    /// Existing Rust destination protection must not treat the same logical
    /// permission input as approval of a newly retargeted physical destination.
    #[cfg(unix)]
    #[tokio::test]
    async fn permission_request_hook_same_edit_path_keeps_existing_destination_guard() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _trust = crate::services::hooks::test_support::SessionTrustGuard::accepted();
        let fixture = PermissionRequestFixture::new("1");
        crate::bootstrap::state::replace_registered_hooks(Default::default());
        let original = fixture.root.join("workspace/original.txt");
        std::fs::write(&original, "old\n").unwrap();
        std::fs::write(&fixture.target, "old\n").unwrap();
        let link = fixture.root.join("workspace/approved-link.txt");
        std::os::unix::fs::symlink(&original, &link).unwrap();
        let mut context = fixture.context();
        context.tools = vec![crate::tools::file_edit_tool::file_edit_tool_schema()];
        record_read(&context, &link);
        let block = edit_block(&link);
        let assistant = assistant(&block);
        let request = run_tool_use(&block, &assistant, &context, &mut Vec::new())
            .request
            .unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&fixture.target, &link).unwrap();
        let prepared = prepare_permission_prompt_hooks_with_config(
            &request,
            &context,
            Some(&allow_input(block.input.clone())),
            Vec::new(),
            Some(&context.abort_controller),
        )
        .await;
        assert_eq!(
            prepared.request.call_input.as_ref().unwrap()
                [crate::tools::file_edit_tool::CHECKED_EDIT_DESTINATION_KEY],
            json!(original)
        );
        let result = streamed_check_permissions_and_call_tool_after_pre_tool_hooks_with_response(
            &prepared.request,
            &PermissionPromptResponse::new(prepared.forced_choice.unwrap()),
            false,
            None,
            &context,
            Some(&assistant),
        )
        .await;
        let message = result.message.expect("actual Edit rejection");
        let result = user_tool_result_block(&message).unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("unexpectedly modified"));
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&fixture.target).unwrap(), "old\n");
        assert!(!context.abort_controller.is_aborted());
    }
    #[tokio::test]
    async fn hook_allow_rule_ask_matches_official_callback_and_preserves_shared_mode() {
        let mut permission = ToolPermissionContext::default();
        permission.always_ask_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Bash", None,
            )],
        );
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(permission.clone()),
                ..Default::default()
            },
            None,
        );
        let assistant = AssistantMessage {
            uuid: "assistant-hook-ask".into(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        for writable in [false, true] {
            let mut context = ToolUseContext::default().with_app_store(store.clone());
            context.app_store.writable = writable;
            context.tools = vec![crate::tools::bash_tool::bash_tool_schema()];
            context.can_use_tool =
                crate::tool::CanUseToolCallback::new(|_, _, context, _, _, forced| {
                    assert!(
                        forced.is_none(),
                        "rule-only Ask must not become forceDecision"
                    );
                    assert_eq!(
                        context
                            .get_app_state()
                            .unwrap()
                            .tool_permission_context
                            .mode,
                        crate::types::permissions::PermissionMode::Default
                    );
                    PermissionDecision::Deny {
                        message: "callback policy".into(),
                        decision_reason:
                            crate::types::permissions::PermissionDecisionReason::Other {
                                reason: "callback".into(),
                            },
                        tool_use_id: Some("callback-id".into()),
                    }
                });
            let request =
                crate::utils::permissions::permissions::mock_permission_request_with_input(
                    "perm",
                    "toolu",
                    "Bash",
                    "custom action",
                    serde_json::json!({"command":"custom-action"}),
                    crate::types::permissions::PermissionMode::Default,
                );
            let result = apply_required_can_use_tool_after_hooks(
                request,
                &context,
                Some(&assistant),
                Some(PermissionPromptChoice::AllowOnce),
                false,
            )
            .await
            .unwrap();
            assert_eq!(result.forced_choice, Some(PermissionPromptChoice::Deny));
            assert_eq!(result.request.message, "callback policy");
            assert_eq!(*store.get().tool_permission_context, permission);
        }
    }

    #[tokio::test]
    async fn hook_ask_matches_official_forced_decision_with_and_without_callback() {
        let assistant = AssistantMessage {
            uuid: "assistant-hook-force".into(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        for with_callback in [false, true] {
            let mut context = ToolUseContext::with_permission_context(ToolPermissionContext {
                mode: crate::types::permissions::PermissionMode::BypassPermissions,
                ..Default::default()
            });
            context.tools = vec![crate::tools::bash_tool::bash_tool_schema()];
            if with_callback {
                context.can_use_tool =
                    crate::tool::CanUseToolCallback::new(|_, _, _, _, _, forced| {
                        forced.expect("hook Ask forceDecision")
                    });
            }
            let mut request =
                crate::utils::permissions::permissions::mock_permission_request_with_input(
                    "perm",
                    "toolu",
                    "Bash",
                    "custom action",
                    serde_json::json!({"command":"custom-action"}),
                    context.tool_permission_context.mode,
                );
            request.permission_result = Some(PermissionDecision::Ask {
                message: "hook requests confirmation".into(),
                updated_input: None,
                decision_reason: Some(crate::types::permissions::PermissionDecisionReason::Hook {
                    hook_name: "PreToolUse:Bash".into(),
                    hook_source: None,
                    reason: None,
                }),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            });
            let result = apply_required_can_use_tool_after_hooks(
                request,
                &context,
                Some(&assistant),
                None,
                false,
            )
            .await
            .unwrap();
            assert!(result.force_ask);
            assert_eq!(result.forced_choice, None);
            assert_eq!(result.request.message, "hook requests confirmation");
        }
    }
    #[tokio::test]
    async fn resolved_callback_abort_matches_official_completed_refusal_and_rejection_identity() {
        let assistant = AssistantMessage {
            uuid: "assistant-cancel".into(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        for rejected in [false, true] {
            let mut context = ToolUseContext::default();
            context.require_can_use_tool = true;
            context.tools = vec![crate::tools::todo_write_tool::todo_write_tool_schema()];
            context.can_use_tool = crate::tool::CanUseToolCallback::new_async_fallible(
                move |_, _, context, _, _, _| {
                    Box::pin(async move {
                        if rejected {
                            return Err(crate::utils::errors::AbortError::new(
                                "canonical rejection",
                            ));
                        }
                        context.abort_controller.abort();
                        Ok(PermissionDecision::Ask {
                            message: "cancelled by user".into(),
                            updated_input: None,
                            decision_reason: None,
                            suggestions: Vec::new(),
                            blocked_path: None,
                            metadata: None,
                            is_bash_security_check_for_misparsing: false,
                            pending_classifier_check: None,
                            content_blocks: Vec::new(),
                        })
                    })
                },
            );
            let request =
                crate::utils::permissions::permissions::mock_permission_request_with_input(
                    "perm",
                    "toolu",
                    "TodoWrite",
                    "todos",
                    serde_json::json!({"todos":[]}),
                    context.tool_permission_context.mode,
                );
            let result = apply_required_can_use_tool_after_hooks(
                request,
                &context,
                Some(&assistant),
                None,
                false,
            )
            .await;
            if rejected {
                assert!(matches!(result, Err(ref error) if error.message == "canonical rejection"));
            } else {
                let result = result.unwrap();
                assert_eq!(result.forced_choice, Some(PermissionPromptChoice::Deny));
                assert!(!result.force_ask);
                assert_eq!(result.request.message, "cancelled by user");
            }
        }
    }
}
