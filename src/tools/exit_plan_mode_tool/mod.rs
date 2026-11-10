//! ExitPlanMode tool metadata, UI, and call path.
//!
//! Maps to:
//! - CC `tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts`
//! - CC `tools/ExitPlanModeTool/constants.ts`
//! - CC `tools/ExitPlanModeTool/prompt.ts`
//! - CC `tools/ExitPlanModeTool/UI.tsx`

pub mod constants;
pub mod prompt;
pub mod ui;

/// Maps to: CC `ExitPlanModeV2Tool.ts:64-73` `allowedPromptSchema`.
fn allowed_prompt_schema() -> crate::utils::zod::Schema {
    use crate::utils::zod;
    zod::object(vec![
        (
            "tool",
            zod::enumeration(vec!["Bash"]).describe("The tool this prompt applies to"),
        ),
        (
            "prompt",
            zod::string().describe(
                "Semantic description of the action, e.g. \"run tests\", \"install dependencies\"",
            ),
        ),
    ])
}

/// Maps to: CC `ExitPlanModeV2Tool.ts:77-89` `inputSchema`.
///
/// This is the schema the model sees: `allowedPrompts` only, plus
/// `.passthrough()` so `normalizeToolInput`'s injected `plan`/`planFilePath`
/// reach `call` without ever being advertised as parameters (the tool prompt
/// states "This tool does NOT take the plan content as a parameter").
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::passthrough_object(vec![(
            "allowedPrompts",
            zod::array(allowed_prompt_schema())
                .optional()
                .describe("Prompt-based permissions needed to implement the plan. These describe categories of actions rather than specific commands."),
        )])
    })
}

/// Maps to: CC `ExitPlanModeV2Tool.ts:97-108` `_sdkInputSchema`.
///
/// SDK/hook-facing only: it extends [`input_schema`] with the fields
/// `normalizeToolInput` injects. It is never handed to the model.
pub fn sdk_input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        zod::passthrough_object(vec![
            (
                "allowedPrompts",
                zod::array(allowed_prompt_schema())
                    .optional()
                    .describe("Prompt-based permissions needed to implement the plan. These describe categories of actions rather than specific commands."),
            ),
            (
                "plan",
                zod::string()
                    .optional()
                    .describe("The plan content (injected by normalizeToolInput from disk)"),
            ),
            (
                "planFilePath",
                zod::string()
                    .optional()
                    .describe("The plan file path (injected by normalizeToolInput)"),
            ),
        ])
    })
}

pub fn exit_plan_mode_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::EXIT_PLAN_MODE_V2_TOOL_NAME.to_string(),
        description: prompt::EXIT_PLAN_MODE_V2_TOOL_PROMPT.to_string(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts` outputSchema (:110).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) plan: Option<String>,
    pub(crate) is_agent: bool,
    pub(crate) file_path: Option<String>,
    pub(crate) has_task_tool: Option<bool>,
    pub(crate) plan_was_edited: Option<bool>,
    pub(crate) awaiting_leader_approval: Option<bool>,
    pub(crate) request_id: Option<String>,
}

fn exit_plan_mode_model_content(output: &Output) -> String {
    // Maps to CC `mapToolResultToToolResultBlockParam` (:419-492).
    if output.awaiting_leader_approval.unwrap_or(false) {
        return format!(
            "Your plan has been submitted to the team lead for approval.\n\nPlan file: {}\n\n**What happens next:**\n1. Wait for the team lead to review your plan\n2. You will receive a message in your inbox with approval/rejection\n3. If approved, you can proceed with implementation\n4. If rejected, refine your plan based on the feedback\n\n**Important:** Do NOT proceed until you receive approval. Check your inbox for response.\n\nRequest ID: {}",
            output.file_path.as_deref().unwrap_or("undefined"),
            output.request_id.as_deref().unwrap_or("undefined"),
        );
    }

    if output.is_agent {
        return "User has approved the plan. There is nothing else needed from you now. Please respond with \"ok\""
            .to_string();
    }

    let Some(plan) = output
        .plan
        .as_deref()
        .filter(|plan| !plan.trim().is_empty())
    else {
        return "User has approved exiting plan mode. You can now proceed.".to_string();
    };

    let team_hint = if output.has_task_tool.unwrap_or(false) {
        "\n\nIf this plan can be broken down into multiple independent tasks, consider using the TeamCreate tool to create a team and parallelize the work."
    } else {
        ""
    };
    let plan_label = if output.plan_was_edited.unwrap_or(false) {
        "Approved Plan (edited by user)"
    } else {
        "Approved Plan"
    };
    format!(
        "User has approved your plan. You can now start coding. Start with updating your todo list if applicable\n\nYour plan has been saved to: {}\nYou can refer back to it if needed during implementation.{team_hint}\n\n## {plan_label}:\n{plan}",
        output.file_path.as_deref().unwrap_or("undefined"),
    )
}

/// Maps to: CC `ExitPlanModeV2Tool.validateInput` (:195-219).
pub(crate) fn validate_exit_plan_mode_input(
    _input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> crate::tool::ValidationResult {
    if crate::utils::teammate::is_teammate() {
        return crate::tool::ValidationResult::Ok;
    }
    let mode = context
        .get_app_state()
        .map(|state| state.tool_permission_context.mode)
        .unwrap_or(context.tool_permission_context.mode);
    if mode != crate::types::permissions::PermissionMode::Plan {
        return crate::tool::ValidationResult::error(
            "You are not in plan mode. This tool is only for exiting plan mode after writing a plan. If your plan was already approved, continue with implementation.",
            1,
        );
    }
    crate::tool::ValidationResult::Ok
}

/// Maps to: CC `ExitPlanModeV2Tool.call` (:243-417) — main session restore +
/// teammate leader-approval mailbox path.
pub(crate) fn exit_plan_mode_call(
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Result<Output, String> {
    use crate::types::permissions::PermissionMode;
    use crate::utils::plans::{get_plan, get_plan_file_path};

    let is_agent = context.agent_id.is_some();
    let file_path = get_plan_file_path(context.agent_id.as_deref());
    let file_path_str = file_path.display().to_string();

    // CC :251-253: `'plan' in input && typeof input.plan === 'string'` — any
    // string counts, including the empty string (an edited-to-empty plan);
    // `??` only falls back to disk on undefined.
    let input_plan = input
        .get("plan")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let plan = input_plan
        .clone()
        .or_else(|| get_plan(context.agent_id.as_deref()));

    // Sync disk when the permission UI injected an edited plan. CC :258-261
    // swallows the write failure (`.catch(e => logError(e))`) and continues.
    if let Some(ref edited) = input_plan
        && let Err(error) = std::fs::write(&file_path, edited)
    {
        tracing::error!("Failed to write plan file {file_path_str}: {error}");
    }

    // Teammate + plan_mode_required → mailbox approval (CC :264-312).
    if crate::utils::teammate::is_teammate() && crate::utils::teammate::is_plan_mode_required() {
        // CC :266 `if (!plan) throw` — JS falsy: an empty-string plan also
        // throws here, unlike the main branch where "" is a valid edit.
        let plan = plan.filter(|plan| !plan.is_empty()).ok_or_else(|| {
            format!(
                "No plan file found at {file_path_str}. Please write your plan to this file before calling ExitPlanMode."
            )
        })?;
        // CC `getAgentName() || 'unknown'` (:271) — JS falsy: the empty
        // string also falls back.
        let agent_name = crate::utils::teammate::get_agent_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        // CC keeps the raw `getTeamName()` for the mailbox write (:287-295);
        // the `|| 'default'` fallback applies only inside formatAgentId's
        // argument (:273-276) — they are two different values.
        let team_name = crate::utils::teammate::get_team_name(None);
        let request_id = crate::utils::agent_id::generate_request_id(
            "plan_approval",
            &crate::utils::agent_id::format_agent_id(
                &agent_name,
                team_name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("default"),
            ),
        );
        // CC `new Date().toISOString()` — millisecond precision with a `Z`
        // suffix, two independent calls.
        let approval_request = serde_json::json!({
            "type": "plan_approval_request",
            "from": agent_name,
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "planFilePath": file_path_str,
            "planContent": plan,
            "requestId": request_id,
        });
        crate::utils::teammate_mailbox::write_to_mailbox(
            "team-lead",
            crate::utils::teammate_mailbox::TeammateMessageInput {
                from: agent_name,
                text: approval_request.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                color: None,
                summary: None,
            },
            team_name.as_deref(),
        )
        .map_err(|error| format!("Failed to submit plan approval request: {error}"))?;
        // SEAM: CC :297-302 then finds the in-process teammate task
        // (`findInProcessTeammateTaskId`) and flips `awaitingPlanApproval` via
        // `setAwaitingPlanApproval`. `utils/inProcessTeammateHelpers.ts` has
        // no Rust port yet (the `awaiting_plan_approval` field exists on the
        // task state); wire this when that file lands.
        return Ok(Output {
            plan: Some(plan),
            is_agent: true,
            file_path: Some(file_path_str),
            has_task_tool: None,
            plan_was_edited: None,
            awaiting_leader_approval: Some(true),
            request_id: Some(request_id),
        });
    }

    // Restore permission mode from prePlanMode.
    // Maps to: CC `ExitPlanModeV2Tool.call` setAppState (:357-403) — NOT
    // `transitionPermissionMode`. Official path hand-writes:
    // setHasExitedPlanMode, setNeedsPlanModeExitAttachment, setAutoModeActive,
    // stripDangerousPermissionsForAutoMode / restoreDangerousPermissions,
    // mode = restoreMode, prePlanMode = undefined.
    if let Some(_store) = context.app_store.store.as_ref() {
        // B3 flip-audit: CC ExitPlanModeV2Tool.ts:358 — `if (mode !== 'plan')
        // return prev`; the side effects below stay after the guard, exactly
        // as CC's :359+ run after the early return.
        // SEAM (pre-existing gap): CC :327-355 computes a
        // gateFallbackNotification + logForDebugging + addNotification
        // ('auto-mode-gate-plan-exit-fallback') BEFORE this setAppState (it
        // runs even on the Same path); that notification block is unported.
        context.set_app_state_decided(|prev| {
            if prev.tool_permission_context.mode != PermissionMode::Plan {
                return crate::state::store::UpdateDecision::Same(());
            }
            crate::bootstrap::state::set_has_exited_plan_mode(true);
            crate::bootstrap::state::set_needs_plan_mode_exit_attachment(true);
            let mut restore_mode = prev
                .tool_permission_context
                .pre_plan_mode
                .unwrap_or(PermissionMode::Default);
            // Circuit-breaker defense (CC): never restore Auto if gate is off.
            if restore_mode == PermissionMode::Auto
                && !crate::utils::permissions::permission_setup::is_auto_mode_gate_enabled()
            {
                restore_mode = PermissionMode::Default;
            }
            let final_restoring_auto = restore_mode == PermissionMode::Auto;
            let auto_was_used_during_plan =
                crate::utils::permissions::auto_mode_state::is_auto_mode_active();
            crate::utils::permissions::auto_mode_state::set_auto_mode_active(final_restoring_auto);
            if auto_was_used_during_plan && !final_restoring_auto {
                crate::bootstrap::state::set_needs_auto_mode_exit_attachment(true);
            }
            let mut base = (*prev.tool_permission_context).clone();
            if final_restoring_auto {
                base = crate::utils::permissions::permission_setup::strip_dangerous_permissions_for_auto_mode(
                    &base,
                );
            } else if base.stripped_dangerous_rules.is_some() {
                base = crate::utils::permissions::permission_setup::restore_dangerous_permissions(
                    &base,
                );
            }
            base.mode = restore_mode;
            base.pre_plan_mode = None;
            let mut next = (**prev).clone();
            next.set_tool_permission_context(base);
            crate::state::store::UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
    } else {
        // No AppStore — still flip session flags for attachment plumbing.
        crate::bootstrap::state::set_has_exited_plan_mode(true);
        crate::bootstrap::state::set_needs_plan_mode_exit_attachment(true);
        crate::utils::permissions::auto_mode_state::set_auto_mode_active(false);
    }

    // CC :405-407: `toolMatchesName(t, AGENT_TOOL_NAME)` — name or alias
    // equals 'Agent' only (Tool.ts:348-353); 'Task' is the LEGACY constant
    // and is not consulted here.
    let has_task_tool = crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        && context
            .tools
            .iter()
            .any(|tool| tool.name == "Agent" || tool.aliases.iter().any(|alias| alias == "Agent"));

    Ok(Output {
        plan,
        is_agent,
        file_path: Some(file_path_str),
        has_task_tool: if has_task_tool { Some(true) } else { None },
        plan_was_edited: if input_plan.is_some() {
            Some(true)
        } else {
            None
        },
        awaiting_leader_approval: None,
        request_id: None,
    })
}

/// Behavioral half of CC `ExitPlanModeTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ExitPlanModeTool;

impl crate::tool::ToolCall for ExitPlanModeTool {
    fn name(&self) -> &'static str {
        "ExitPlanMode"
    }

    /// Maps to: CC `ExitPlanModeV2Tool.ts:154-156` `async prompt() { return
    /// EXIT_PLAN_MODE_V2_TOOL_PROMPT }` — same source the wire schema renders.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::EXIT_PLAN_MODE_V2_TOOL_PROMPT.to_string()
    }

    fn search_hint(&self) -> Option<&'static str> {
        Some("present plan for approval and start coding (plan mode only)")
    }

    /// Maps to: CC `ExitPlanModeV2Tool.ts:150` `maxResultSizeChars`.
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `ExitPlanModeV2Tool.ts:151-153` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        "Prompts the user to exit plan mode and start coding".to_string()
    }

    /// Maps to: CC `ExitPlanModeV2Tool.ts:163-165` `userFacingName()` — the
    /// empty string.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        String::new()
    }

    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `ExitPlanModeV2Tool.isEnabled()` channels guard (:167-178):
    /// `(feature('KAIROS') || feature('KAIROS_CHANNELS')) &&
    /// getAllowedChannels().length > 0` → false. These are build features,
    /// not the `tengu_harbor` GrowthBook gate. Paired with the same gate on
    /// EnterPlanMode so plan mode isn't a trap.
    fn is_enabled(&self) -> bool {
        let kairos_built = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::Kairos,
        ) || crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::KairosChannels,
        );
        if kairos_built && !crate::bootstrap::state::get_allowed_channels().is_empty() {
            return false;
        }
        true
    }

    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `isReadOnly() { return false }` — now writes to disk.
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Maps to: CC `requiresUserInteraction` — teammates skip local UI.
    fn requires_user_interaction(&self) -> bool {
        !crate::utils::teammate::is_teammate()
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        validate_exit_plan_mode_input(args, context)
    }

    /// Maps to: CC `checkPermissions` (:221-238).
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        if crate::utils::teammate::is_teammate() {
            return crate::utils::permissions::permission_result::PermissionResult::Allow {
                updated_input: Some(args.clone()),
                user_modified: None,
                decision_reason: None,
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            };
        }
        crate::utils::permissions::permission_result::PermissionResult::Ask {
            message: "Exit plan mode?".to_string(),
            updated_input: None,
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = request;
            match exit_plan_mode_call(args, context) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::ExitPlanMode(output),
                    new_messages: Vec::new(),
                },
                Err(message) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::Composed {
                        content: format!("Error: {message}"),
                        status: crate::types::message::ToolResultStatus::Error,
                    },
                    new_messages: Vec::new(),
                },
            }
        })
    }

    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::ExitPlanMode(output) => (
                exit_plan_mode_model_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>ExitPlanMode returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording ExitPlanModeV2Tool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::ExitPlanMode(output) => Some(ui::output_to_value(output)),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_plan_mode_tool_schema_matches_official_v2_input_shape() {
        let schema = exit_plan_mode_tool_schema();
        assert_eq!(schema.name, "ExitPlanMode");
        assert!(schema.input_schema.get("required").is_none());
        let allowed_prompt = &schema.input_schema["properties"]["allowedPrompts"]["items"];
        assert_eq!(
            allowed_prompt["required"],
            serde_json::json!(["tool", "prompt"])
        );
        assert_eq!(
            allowed_prompt["properties"]["tool"]["enum"],
            serde_json::json!(["Bash"])
        );
        // The model-facing schema advertises `allowedPrompts` only; `.passthrough()`
        // is what lets `normalizeToolInput`'s injected fields reach `call`.
        assert_eq!(
            schema.input_schema["properties"]
                .as_object()
                .map(|properties| properties.len()),
            Some(1)
        );
        assert_eq!(
            schema.input_schema["additionalProperties"],
            serde_json::json!({})
        );
        assert!(schema.description.contains("Before Using This Tool"));

        // `_sdkInputSchema` is the separate export that adds them back.
        let sdk = crate::utils::zod::to_json_schema(sdk_input_schema());
        assert!(sdk["properties"].get("plan").is_some());
        assert!(sdk["properties"].get("planFilePath").is_some());
        assert!(sdk.get("required").is_none());
    }

    #[tokio::test]
    async fn exit_plan_mode_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;
        use crate::types::permissions::PermissionMode;

        let args = serde_json::json!({
            "plan": "## Plan\nShip it",
            "planFilePath": "/tmp/plan.md"
        });
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-exit-plan".to_string(),
            "toolu_exit_plan".to_string(),
            "ExitPlanMode".to_string(),
            "{}".to_string(),
            args.clone(),
            PermissionMode::Plan,
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.tool_permission_context.mode = PermissionMode::Plan;
        let tool = ExitPlanModeTool;
        let result = tool.call(&args, &request, &context, None, None, None).await;

        let crate::tool::ToolOutput::ExitPlanMode(output) = result.data else {
            panic!("ExitPlanMode should return its official ToolOutput variant");
        };
        assert_eq!(output.plan.as_deref(), Some("## Plan\nShip it"));
        assert!(crate::bootstrap::state::has_exited_plan_mode_in_session());
        assert!(crate::bootstrap::state::needs_plan_mode_exit_attachment());

        let data = crate::tool::ToolOutput::ExitPlanMode(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_exit_plan");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert!(content.contains("User has approved your plan"));
        assert!(content.contains("## Approved Plan (edited by user):"));

        // Explicit CC member values (ExitPlanModeV2Tool.ts:150-153, :163-165).
        assert_eq!(tool.max_result_size_chars(), 100_000);
        assert_eq!(tool.user_facing_name(None), "");
        assert_eq!(
            tool.description(&serde_json::json!({})),
            "Prompts the user to exit plan mode and start coding"
        );
    }

    #[test]
    fn exit_plan_mode_empty_string_plan_still_counts_as_an_edit() {
        // CC :251-253 accepts any string — the `??` disk fallback only fires
        // on undefined, so an edited-to-empty plan stays "" and flags
        // planWasEdited.
        let context = crate::tool::ToolUseContext::default();
        let out = exit_plan_mode_call(&serde_json::json!({"plan": ""}), &context)
            .expect("empty-string plan is a valid edit");
        assert_eq!(out.plan.as_deref(), Some(""));
        assert_eq!(out.plan_was_edited, Some(true));
        // Empty plan takes the simplified copy (CC map :462-468).
        assert_eq!(
            super::exit_plan_mode_model_content(&out),
            "User has approved exiting plan mode. You can now proceed."
        );
    }

    #[test]
    fn validate_input_rejects_outside_plan_mode() {
        let context = crate::tool::ToolUseContext::default();
        let result = validate_exit_plan_mode_input(&serde_json::json!({}), &context);
        match result {
            crate::tool::ValidationResult::Error { error_code, .. } => {
                assert_eq!(error_code, 1);
            }
            crate::tool::ValidationResult::Ok => panic!("expected reject outside plan"),
            crate::tool::ValidationResult::Fatal { message } => {
                panic!("unexpected fatal validation: {message}")
            }
        }
    }

    #[test]
    fn exit_with_app_store_restores_pre_plan_mode() {
        use crate::state::app_state_store::AppState;
        use crate::state::store::AppStore;
        use crate::types::permissions::PermissionMode;

        let mut initial = AppState::default();
        initial.set_tool_permission_context(crate::tool::ToolPermissionContext {
            mode: PermissionMode::Plan,
            pre_plan_mode: Some(PermissionMode::AcceptEdits),
            ..Default::default()
        });
        let store = AppStore::new(initial, None);
        let context = crate::tool::ToolUseContext::default().with_app_store(store.clone());

        let out =
            exit_plan_mode_call(&serde_json::json!({"plan": "do things"}), &context).expect("exit");
        assert_eq!(out.plan.as_deref(), Some("do things"));
        let state = store.get();
        assert_eq!(
            state.tool_permission_context.mode,
            PermissionMode::AcceptEdits
        );
        assert_eq!(state.tool_permission_context.pre_plan_mode, None);
    }
}
