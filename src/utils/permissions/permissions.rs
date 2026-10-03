//! Maps to: CC `utils/permissions/permissions.ts`.
//! This is the permission decision/helper seam. It can build permission
//! requests, apply prompt choices to an in-memory context, and produce
//! transcript text; it never executes tools, and rule persistence remains a
//! separate settings-policy seam from live session transcript writes.

use super::permission_result::{PermissionDecisionReason, PermissionResult};
use super::permission_update::apply_permission_updates;
use super::shell_rule_matching::permission_rule_content_matches;
use crate::services::mcp::mcp_string_utils::{
    get_tool_name_for_permission_check, mcp_info_from_string,
};
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionMode, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest, PermissionRule, PermissionRuleSource, PermissionRuleValue, PermissionUpdate,
    PermissionUpdateDestination, PromptDecision, ToolPermissionRulesBySource,
};
use crate::types::tools::McpToolInfo;
use indexmap::IndexMap;
use std::collections::HashSet;
pub fn mock_permission_request(
    id: impl Into<String>,
    tool_use_id: impl Into<String>,
    tool_name: impl Into<String>,
    input_summary: impl Into<String>,
    mode: PermissionMode,
) -> PermissionRequest {
    let input_summary = input_summary.into();
    mock_permission_request_with_input(
        id,
        tool_use_id,
        tool_name,
        input_summary.clone(),
        serde_json::json!({ "summary": input_summary }),
        mode,
    )
}

pub fn mock_permission_request_with_input(
    id: impl Into<String>,
    tool_use_id: impl Into<String>,
    tool_name: impl Into<String>,
    input_summary: impl Into<String>,
    input: serde_json::Value,
    mode: PermissionMode,
) -> PermissionRequest {
    let tool_name = tool_name.into();
    let input_summary = input_summary.into();
    PermissionRequest {
        permission_result: None,
        id: id.into(),
        tool_use_id: tool_use_id.into(),
        description: String::new(),
        message: String::new(),
        rule: PermissionRuleValue::new(tool_name.clone(), Some(input_summary.clone())),
        decision_reason: None,
        tool_name,
        mcp_info: None,
        input_summary,
        input,
        call_input: None,
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode,
    }
}

/// Maps to: CC `utils/permissions/permissions.ts:109-129#PERMISSION_RULE_SOURCES`.
pub const PERMISSION_RULE_SOURCES: [PermissionRuleSource; 8] = [
    PermissionRuleSource::UserSettings,
    PermissionRuleSource::ProjectSettings,
    PermissionRuleSource::LocalSettings,
    PermissionRuleSource::FlagSettings,
    PermissionRuleSource::PolicySettings,
    PermissionRuleSource::CliArg,
    PermissionRuleSource::Command,
    PermissionRuleSource::Session,
];

/// Maps to: CC `utils/permissions/permissions.ts:122-131#getAllowRules`.
pub fn get_allow_rules(context: &ToolPermissionContext) -> Vec<PermissionRule> {
    PERMISSION_RULE_SOURCES
        .iter()
        .flat_map(|source| {
            context
                .always_allow_rules
                .get(source)
                .into_iter()
                .flatten()
                .cloned()
                .map(|rule_value| PermissionRule {
                    source: *source,
                    rule_behavior: PermissionBehavior::Allow,
                    rule_value,
                })
        })
        .collect()
}

/// Maps to: CC `utils/permissions/permissions.ts:213-221#getDenyRules`.
pub fn get_deny_rules(context: &ToolPermissionContext) -> Vec<PermissionRule> {
    PERMISSION_RULE_SOURCES
        .iter()
        .flat_map(|source| {
            context
                .always_deny_rules
                .get(source)
                .into_iter()
                .flatten()
                .cloned()
                .map(|rule_value| PermissionRule {
                    source: *source,
                    rule_behavior: PermissionBehavior::Deny,
                    rule_value,
                })
        })
        .collect()
}

/// Maps to: CC `utils/permissions/permissions.ts:223-231#getAskRules`.
pub fn get_ask_rules(context: &ToolPermissionContext) -> Vec<PermissionRule> {
    PERMISSION_RULE_SOURCES
        .iter()
        .flat_map(|source| {
            context
                .always_ask_rules
                .get(source)
                .into_iter()
                .flatten()
                .cloned()
                .map(|rule_value| PermissionRule {
                    source: *source,
                    rule_behavior: PermissionBehavior::Ask,
                    rule_value,
                })
        })
        .collect()
}

pub fn has_in_memory_allow_rule(
    context: &ToolPermissionContext,
    rule: &PermissionRuleValue,
) -> bool {
    has_matching_rule(&context.always_allow_rules, rule)
}

pub fn has_in_memory_deny_rule(
    context: &ToolPermissionContext,
    rule: &PermissionRuleValue,
) -> bool {
    has_matching_rule(&context.always_deny_rules, rule)
}

pub fn has_in_memory_ask_rule(context: &ToolPermissionContext, rule: &PermissionRuleValue) -> bool {
    has_matching_rule(&context.always_ask_rules, rule)
}

/// Maps to: CC `utils/permissions/permissions.ts:305-319#getDenyRuleForAgent`.
pub fn get_deny_rule_for_agent(
    context: &ToolPermissionContext,
    agent_tool_name: &str,
    agent_type: &str,
) -> Option<PermissionRule> {
    get_deny_rules(context).into_iter().find(|rule| {
        rule.rule_value.tool_name == agent_tool_name
            && rule.rule_value.rule_content.as_deref() == Some(agent_type)
    })
}

/// Maps to: CC `utils/permissions/permissions.ts:324-345#filterDeniedAgents`.
pub fn filter_denied_agents<T: Clone>(
    agents: &[T],
    context: &ToolPermissionContext,
    agent_tool_name: &str,
    agent_type: impl Fn(&T) -> &str,
) -> Vec<T> {
    let denied_agent_types = get_deny_rules(context)
        .into_iter()
        .filter(|rule| rule.rule_value.tool_name == agent_tool_name)
        .filter_map(|rule| rule.rule_value.rule_content)
        .collect::<HashSet<_>>();
    agents
        .iter()
        .filter(|agent| !denied_agent_types.contains(agent_type(agent)))
        .cloned()
        .collect()
}

/// Maps to: CC `utils/permissions/permissions.ts:121-125#permissionRuleSourceDisplayString`.
pub fn permission_rule_source_display_string(source: PermissionRuleSource) -> &'static str {
    match source {
        PermissionRuleSource::UserSettings => "user settings",
        PermissionRuleSource::ProjectSettings => "shared project settings",
        PermissionRuleSource::LocalSettings => "project local settings",
        PermissionRuleSource::FlagSettings => "command line arguments",
        PermissionRuleSource::PolicySettings => "enterprise managed settings",
        PermissionRuleSource::CliArg => "CLI argument",
        PermissionRuleSource::Command => "command configuration",
        PermissionRuleSource::Session => "current session",
    }
}

/// Maps to: CC `utils/permissions/permissions.ts:357-391#getRuleByContentsForToolName`.
/// Later-source duplicate contents replace attribution without moving the key.
pub fn get_rule_by_contents_for_tool_name(
    context: &ToolPermissionContext,
    tool_name: &str,
    behavior: PermissionBehavior,
) -> IndexMap<String, PermissionRule> {
    let mut rules_by_content = IndexMap::new();
    let rules = match behavior {
        PermissionBehavior::Allow => get_allow_rules(context),
        PermissionBehavior::Deny => get_deny_rules(context),
        PermissionBehavior::Ask => get_ask_rules(context),
    };
    for rule in rules {
        if rule.rule_value.tool_name == tool_name {
            if let Some(content) = rule.rule_value.rule_content.as_ref() {
                rules_by_content.insert(content.clone(), rule);
            }
        }
    }
    rules_by_content
}

/// Maps to CC `createPermissionRequestMessage(toolName, decisionReason)`.
pub fn create_permission_request_message(
    tool_name: &str,
    decision_reason: Option<&PermissionDecisionReason>,
) -> String {
    let Some(reason) = decision_reason else {
        return format!(
            "Claude requested permissions to use {tool_name}, but you haven't granted it yet."
        );
    };
    match reason {
        PermissionDecisionReason::Classifier { classifier, reason }
            if cfg!(feature = "anthropic_internal") =>
        {
            format!(
                "Classifier '{classifier}' requires approval for this {tool_name} command: {reason}"
            )
        }
        PermissionDecisionReason::Hook {
            hook_name, reason, ..
        } => reason.as_ref().map_or_else(
            || format!("Hook '{hook_name}' requires approval for this {tool_name} command"),
            |reason| format!("Hook '{hook_name}' blocked this action: {reason}"),
        ),
        PermissionDecisionReason::Rule { rule } => format!(
            "Permission rule '{}' from {} requires approval for this {tool_name} command",
            rule.rule_value.display(),
            permission_rule_source_display_string(rule.source)
        ),
        PermissionDecisionReason::SubcommandResults { reasons } => {
            let needs_approval = reasons
                .iter()
                .filter_map(|(command, result)| {
                    matches!(
                        result.as_ref(),
                        PermissionResult::Ask { .. } | PermissionResult::Passthrough { .. }
                    )
                    .then(|| {
                        if tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME {
                            let extracted =
                                crate::utils::bash::commands::extract_output_redirections(command);
                            if extracted.redirections.is_empty() {
                                command.clone()
                            } else {
                                extracted.command_without_redirections
                            }
                        } else {
                            command.clone()
                        }
                    })
                })
                .collect::<Vec<_>>();
            if needs_approval.is_empty() {
                format!(
                    "This {tool_name} command contains multiple operations that require approval"
                )
            } else {
                let count = needs_approval.len();
                format!(
                    "This {tool_name} command contains multiple operations. The following {} {} approval: {}",
                    crate::utils::string_utils::plural(count, "part", None),
                    crate::utils::string_utils::plural(count, "requires", Some("require")),
                    needs_approval.join(", ")
                )
            }
        }
        PermissionDecisionReason::PermissionPromptTool {
            permission_prompt_tool_name,
            ..
        } => format!(
            "Tool '{permission_prompt_tool_name}' requires approval for this {tool_name} command"
        ),
        PermissionDecisionReason::SandboxOverride { .. } => {
            "Run outside of the sandbox".to_string()
        }
        PermissionDecisionReason::WorkingDir { reason }
        | PermissionDecisionReason::SafetyCheck { reason, .. }
        | PermissionDecisionReason::Other { reason }
        | PermissionDecisionReason::AsyncAgent { reason } => reason.clone(),
        PermissionDecisionReason::Mode { mode } => format!(
            "Current permission mode ({}) requires approval for this {tool_name} command",
            super::permission_mode::permission_mode_title(*mode)
        ),
        PermissionDecisionReason::Classifier { .. } => format!(
            "Claude requested permissions to use {tool_name}, but you haven't granted it yet."
        ),
    }
}

/// Result of the Rust `hasPermissionsToUseTool(...)` permission gate.
/// Maps to: CC `utils/permissions/permissions.ts:473` — `hasPermissionsToUseTool`
/// is declared as a `CanUseToolFn`, whose return type is `PermissionDecision`
/// (`hooks/useCanUseTool.tsx:44-53`) — so the allow arm is CC's
/// `PermissionAllowDecision` (`types/permissions.ts:174-184`) and carries ALL
/// six of its fields: `updatedInput` / `userModified` / `decisionReason` /
/// `toolUseID` / `acceptFeedback` / `contentBlocks`.
///
/// `Allow` was a UNIT variant until #142: `process_tool_permission_result`
/// flattened the tool's full Allow into it, so a tool-approved
/// `updatedInput` never reached execution — CC's `getUpdatedInputOrFallback`
/// contract could not hold. #170 widened it from three fields to the full CC
/// shape: projecting a canonical [`crate::types::permissions::PermissionDecision`]
/// into this type used to drop `toolUseID` / `acceptFeedback` /
/// `contentBlocks` (the SDK permission-prompt normalizer is a real producer of
/// `toolUseID`, `PermissionPromptToolResultSchema.ts:112-116`). Most allow
/// paths carry no payload ([`HasPermissionsToUseToolResult::allow`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HasPermissionsToUseToolResult {
    Allow {
        updated_input: Option<serde_json::Value>,
        user_modified: Option<bool>,
        decision_reason: Option<PermissionDecisionReason>,
        /// CC `PermissionAllowDecision.toolUseID` (`types/permissions.ts:181`).
        tool_use_id: Option<String>,
        /// CC `PermissionAllowDecision.acceptFeedback` (`types/permissions.ts:182`).
        accept_feedback: Option<String>,
        /// CC `PermissionAllowDecision.contentBlocks` (`types/permissions.ts:183`).
        content_blocks: Vec<serde_json::Value>,
    },
    Deny(PermissionRequest),
    Ask(PermissionRequest),
    /// L1 rejected Promise carrier; never a permission denial.
    Aborted(crate::utils::errors::AbortError),
}

impl HasPermissionsToUseToolResult {
    /// L1 transport projection of CC's complete `PermissionDecision`.
    /// Ask/Deny retain the original union until the actual prompt boundary.
    pub fn into_decision(
        self,
    ) -> Result<crate::types::permissions::PermissionDecision, crate::utils::errors::AbortError>
    {
        use crate::types::permissions::PermissionDecision;
        Ok(match self {
            Self::Aborted(error) => return Err(error),
            Self::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            } => PermissionDecision::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            },
            Self::Deny(request) => {
                let tool_use_id = match request.permission_result {
                    Some(PermissionDecision::Deny { tool_use_id, .. }) => tool_use_id,
                    _ => None,
                };
                PermissionDecision::Deny {
                    message: request.message,
                    decision_reason: request.decision_reason.unwrap_or(
                        PermissionDecisionReason::Other {
                            reason: String::new(),
                        },
                    ),
                    tool_use_id,
                }
            }
            Self::Ask(request) => {
                let (
                    updated_input,
                    is_bash_security_check_for_misparsing,
                    pending_classifier_check,
                    content_blocks,
                ) = match request.permission_result {
                    Some(PermissionDecision::Ask {
                        updated_input,
                        is_bash_security_check_for_misparsing,
                        pending_classifier_check,
                        content_blocks,
                        ..
                    }) => (
                        updated_input,
                        is_bash_security_check_for_misparsing,
                        pending_classifier_check,
                        content_blocks,
                    ),
                    _ => (None, false, None, Vec::new()),
                };
                PermissionDecision::Ask {
                    message: request.message,
                    updated_input,
                    decision_reason: request.decision_reason,
                    suggestions: request.suggestions,
                    blocked_path: request.blocked_path,
                    metadata: request.metadata,
                    is_bash_security_check_for_misparsing,
                    pending_classifier_check,
                    content_blocks,
                }
            }
        })
    }

    /// An allow with no payload — every CC `{ behavior: 'allow' }` literal
    /// whose optional fields are absent.
    pub fn allow() -> Self {
        Self::Allow {
            updated_input: None,
            user_modified: None,
            decision_reason: None,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }
}

/// Inputs for the Rust `hasPermissionsToUseTool(...)` seam.
/// Maps to: CC `hasPermissionsToUseTool(tool, input, context, assistantMessage, toolUseID)`.
pub struct HasPermissionsToUseToolParams<'a> {
    pub tool_use_id: &'a str,
    pub tool_name: &'a str,
    /// Maps to: CC `Tool.mcpInfo` reaching `toolMatchesRule(tool, rule)`.
    /// `None` for builtins and for callers that only know a tool name.
    pub mcp_info: Option<&'a McpToolInfo>,
    pub input_summary: &'a str,
    pub input: &'a serde_json::Value,
    pub context: &'a ToolPermissionContext,
    /// Conversation history for auto-mode classifier transcript.
    /// Maps to: CC `context.messages` passed into `classifyYoloAction`.
    /// Empty when caller has no transcript (skill shell checks, etc.).
    pub messages: &'a [crate::types::message::Message],
    /// Optional AppStore for persisting `AppState.denialTracking`
    /// (CC `context.setAppState({ denialTracking })`).
    pub app_store: Option<&'a crate::state::store::AppStore>,
    /// Maps to CC `ToolUseContext.localDenialTracking` for isolated subagents.
    /// A clone of the context's handle, not a borrow: CC reads it off `context`
    /// at each use (`permissions.ts:490`, `:556`) and writes it in place with
    /// `Object.assign` (`:967-968`), which a `&mut` cannot express from behind
    /// the `&ToolUseContext` the permission path holds.
    pub local_denial_tracking: Option<crate::tool::SharedDenialTracking>,
    /// Maps to: CC turn abort / `classifyYoloAction(..., signal)`.
    /// When set, side_query honors cancellation (Escape).
    pub abort_signal: Option<anthropic_sdk::AbortSignal>,
}

/// Owned callback carrier of CC `canUseTool: hasPermissionsToUseTool`.
/// Projects the existing permission result into CanUseToolFn's canonical union;
/// it deliberately does not call the interactive useCanUseTool wrapper.
pub fn has_permissions_to_use_tool_callback() -> crate::tool::CanUseToolCallback {
    crate::tool::CanUseToolCallback::new_async_fallible(
        |tool, input, context, _assistant, id, _forced| {
            Box::pin(async move {
                let state = context.get_app_state();
                let permission = state
                    .as_ref()
                    .map(|s| s.tool_permission_context.as_ref())
                    .unwrap_or(&context.tool_permission_context);
                let result = has_permissions_to_use_tool_async_with_context(
                    HasPermissionsToUseToolParams {
                        tool_use_id: id,
                        tool_name: &tool.name,
                        mcp_info: tool.mcp_info.as_ref(),
                        input_summary: "",
                        input,
                        context: permission,
                        messages: &context.messages,
                        app_store: context.app_store.store.as_ref(),
                        local_denial_tracking: context.local_denial_tracking.clone(),
                        abort_signal: Some(context.abort_controller.signal()),
                    },
                    Some(context),
                )
                .await;
                result.into_decision()
            })
        },
    )
}

/// Core permission decision logic (sync entry — **production path today**).
/// Maps to: CC `utils/permissions/permissions.ts` `hasPermissionsToUseTool(...)`
/// and `hasPermissionsToUseToolInner(...)`. React/queue handling remains in
/// `hooks/use_can_use_tool.rs`, matching official `useCanUseTool.tsx`.
///
/// Production `tool_execution` / `run_tools` still call this (sync). Classifier
/// uses a bounded `block_on` bridge. Prefer
/// [`has_permissions_to_use_tool_async`] when wiring new async call sites
/// (R3b debt: gate not yet fully async end-to-end).
pub fn has_permissions_to_use_tool(
    params: HasPermissionsToUseToolParams<'_>,
) -> HasPermissionsToUseToolResult {
    has_permissions_to_use_tool_with_context(params, None)
}

/// Full-context entry used by tool orchestration. CC passes `ToolUseContext`
/// directly to `tool.checkPermissions`; retaining it here is required for
/// context-local cwd and AppState decisions (notably Write path rules).
pub fn has_permissions_to_use_tool_with_context(
    params: HasPermissionsToUseToolParams<'_>,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    // CC permissions.ts:1163-1165: an already aborted call rejects before rules.
    if params
        .abort_signal
        .as_ref()
        .is_some_and(|signal| signal.is_aborted())
        || tool_use_context.is_some_and(|context| context.abort_controller.is_aborted())
    {
        return HasPermissionsToUseToolResult::Aborted(crate::utils::errors::AbortError::default());
    }
    // CC permissions.ts:1167 reads getAppState before whole-tool deny/ask.
    let initial_state = tool_use_context.and_then(crate::tool::ToolUseContext::get_app_state);
    let params = HasPermissionsToUseToolParams {
        context: initial_state
            .as_ref()
            .map(|state| state.tool_permission_context.as_ref())
            .unwrap_or(params.context),
        ..params
    };
    let inner = has_permissions_to_use_tool_inner(params, tool_use_context);
    // CC permissions.ts:486,506: the outer decision reads after the tool check.
    let current_state = tool_use_context.and_then(crate::tool::ToolUseContext::get_app_state);
    let outcome = match inner {
        PreClassifierOutcome::Done { params, result } => {
            let params = HasPermissionsToUseToolParams {
                context: current_state
                    .as_ref()
                    .map(|state| state.tool_permission_context.as_ref())
                    .unwrap_or(params.context),
                ..params
            };
            match result {
                HasPermissionsToUseToolResult::Ask(request) => {
                    maybe_classifier_for_ask(params, request)
                }
                result => PreClassifierOutcome::Done { params, result },
            }
        }
        other => other,
    };
    match outcome {
        PreClassifierOutcome::Done { params, result } => {
            reset_consecutive_denials_on_allow(params, &result);
            result
        }
        PreClassifierOutcome::NeedsClassifier { params, request } => {
            apply_auto_mode_classifier_to_ask(params, request, tool_use_context)
        }
        PreClassifierOutcome::NeedsHeadlessHooks { params, request } => {
            resolve_headless_ask_sync(params, request, tool_use_context)
        }
    }
}

/// Async entry matching CC `hasPermissionsToUseTool` (fully async classify).
/// Maps to: CC `await hasPermissionsToUseTool(...)` / `await classifyYoloAction`.
pub async fn has_permissions_to_use_tool_async(
    params: HasPermissionsToUseToolParams<'_>,
) -> HasPermissionsToUseToolResult {
    has_permissions_to_use_tool_async_with_context(params, None).await
}

/// Async full-context permission entry; see [`has_permissions_to_use_tool_with_context`].
pub async fn has_permissions_to_use_tool_async_with_context(
    params: HasPermissionsToUseToolParams<'_>,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    // CC permissions.ts:1163-1165: an already aborted call rejects before rules.
    if params
        .abort_signal
        .as_ref()
        .is_some_and(|signal| signal.is_aborted())
        || tool_use_context.is_some_and(|context| context.abort_controller.is_aborted())
    {
        return HasPermissionsToUseToolResult::Aborted(crate::utils::errors::AbortError::default());
    }
    // CC permissions.ts:1167 reads getAppState before whole-tool deny/ask.
    let initial_state = tool_use_context.and_then(crate::tool::ToolUseContext::get_app_state);
    let params = HasPermissionsToUseToolParams {
        context: initial_state
            .as_ref()
            .map(|state| state.tool_permission_context.as_ref())
            .unwrap_or(params.context),
        ..params
    };
    let inner = has_permissions_to_use_tool_inner(params, tool_use_context);
    // CC permissions.ts:486,506: the outer decision reads after the tool check.
    let current_state = tool_use_context.and_then(crate::tool::ToolUseContext::get_app_state);
    let outcome = match inner {
        PreClassifierOutcome::Done { params, result } => {
            let params = HasPermissionsToUseToolParams {
                context: current_state
                    .as_ref()
                    .map(|state| state.tool_permission_context.as_ref())
                    .unwrap_or(params.context),
                ..params
            };
            match result {
                HasPermissionsToUseToolResult::Ask(request) => {
                    maybe_classifier_for_ask(params, request)
                }
                result => PreClassifierOutcome::Done { params, result },
            }
        }
        other => other,
    };
    match outcome {
        PreClassifierOutcome::Done { params, result } => {
            reset_consecutive_denials_on_allow(params, &result);
            result
        }
        PreClassifierOutcome::NeedsClassifier { params, request } => {
            apply_auto_mode_classifier_to_ask_async(params, request, tool_use_context).await
        }
        PreClassifierOutcome::NeedsHeadlessHooks { params, request } => {
            resolve_headless_ask_async(params, request, tool_use_context).await
        }
    }
}

/// Maps to: CC `utils/permissions/permissions.ts:483-501` — the allow branch of
/// the OUTER `hasPermissionsToUseTool` (`:473-951`), which runs on whatever
/// `hasPermissionsToUseToolInner` returned at `:480`:
///
/// ```ts
/// // Reset consecutive denials on any allowed tool use in auto mode.
/// // This ensures that a successful tool use (even one auto-allowed by rules)
/// // breaks the consecutive denial streak.
/// if (result.behavior === 'allow') {
///   const appState = context.getAppState()
///   if (feature('TRANSCRIPT_CLASSIFIER')) {
///     const currentDenialState =
///       context.localDenialTracking ?? appState.denialTracking
///     if (appState.toolPermissionContext.mode === 'auto' &&
///         currentDenialState && currentDenialState.consecutiveDenials > 0) {
///       const newDenialState = recordSuccess(currentDenialState)
///       persistDenialState(context, newDenialState)
///     }
///   }
///   return result
/// }
/// ```
///
/// Called from both outer entries because the port splits CC's single `async`
/// owner into a sync bridge and an async twin; the block itself is not
/// duplicated. It deliberately does NOT run on anything the auto branch
/// produces — CC reaches that branch only from the `ask` arm at `:505`, below
/// this one, and each of its allow paths owns its own `recordSuccess`:
/// `:621-622` (acceptEdits fast-path) and `:661-662` (safe-tool allowlist), both
/// in `auto_mode_pre_classifier_fast_path`, and `:914-916` (classifier success)
/// in `resolve_auto_mode_classifier_decision`. CC's four `recordSuccess` sites
/// are `:496`, `:621`, `:661`, `:915`; this function is `:496`. They cannot
/// overlap: an `allow` returns at `:500`.
fn reset_consecutive_denials_on_allow(
    params: HasPermissionsToUseToolParams<'_>,
    result: &HasPermissionsToUseToolResult,
) {
    if !matches!(result, HasPermissionsToUseToolResult::Allow { .. }) {
        return;
    }
    if !crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
        return;
    }
    // CC `context.localDenialTracking ?? appState.denialTracking` — nullish
    // coalescing, so a present local state wins even with zero counters, and an
    // absent pair leaves `currentDenialState` undefined (falsy → no reset).
    let current_denial_state = params
        .local_denial_tracking
        .as_ref()
        .map(crate::tool::SharedDenialTracking::get)
        .or_else(|| {
            params
                .app_store
                .and_then(|store| store.get().denial_tracking)
        });
    // CC reads `appState.toolPermissionContext.mode`; this port passes that same
    // permission context in as `params.context` (see `should_try_auto_mode_classifier`).
    // Only `auto` — the `plan` + `isAutoModeActive()` pair is not part of this branch.
    if params.context.mode != PermissionMode::Auto {
        return;
    }
    let Some(current_denial_state) = current_denial_state else {
        return;
    };
    if current_denial_state.consecutive_denials == 0 {
        return;
    }
    let new_denial_state =
        crate::utils::permissions::denial_tracking::record_success(current_denial_state);
    persist_denial_tracking(
        params.app_store,
        params.local_denial_tracking.as_ref(),
        new_denial_state,
    );
}

enum PreClassifierOutcome<'a> {
    /// CC's outer `hasPermissionsToUseTool` still holds `context` after
    /// `hasPermissionsToUseToolInner` returns and reads
    /// `context.localDenialTracking` / `context.getAppState()` from it
    /// (`permissions.ts:480-501`). The Rust inner consumes `params` — the
    /// classifier arm moves them — so `Done` hands them back to the outer entry
    /// for that same branch.
    Done {
        params: HasPermissionsToUseToolParams<'a>,
        result: HasPermissionsToUseToolResult,
    },
    NeedsClassifier {
        params: HasPermissionsToUseToolParams<'a>,
        request: PermissionRequest,
    },
    /// CC `permissions.ts:929-952` — the non-auto ask tail when permission
    /// prompts are unavailable (background/headless agents): PermissionRequest
    /// hooks get one chance to decide, otherwise the ask auto-denies with the
    /// `asyncAgent` reason and `AUTO_REJECT_MESSAGE`. Hook execution is async,
    /// so the sync bridge and the async twin each own the await.
    NeedsHeadlessHooks {
        params: HasPermissionsToUseToolParams<'a>,
        request: PermissionRequest,
    },
}

/// Maps to CC `permissions.ts:1071-1157#checkRuleBasedPermissions`.
/// This deliberately has no mode override, classifier, or AppState mutation:
/// a PreToolUse allow is still subject to explicit deny/ask and safety rules.
pub async fn check_rule_based_permissions(
    tool: &crate::types::tools::Tool,
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Option<crate::types::permissions::PermissionDecision> {
    let state = context.get_app_state();
    let permissions = state
        .as_ref()
        .map(|s| s.tool_permission_context.as_ref())
        .unwrap_or(&context.tool_permission_context);
    let requested_rule = PermissionRuleValue::new(permission_rule_tool_name(&tool.name), None);
    if let Some(rule) = get_deny_rule_for_tool(permissions, &requested_rule, tool.mcp_info.as_ref())
    {
        return Some(crate::types::permissions::PermissionDecision::Deny {
            message: tool_denied_message(&tool.name),
            decision_reason: PermissionDecisionReason::Rule { rule },
            tool_use_id: None,
        });
    }
    if let Some(rule) = get_ask_rule_for_tool(permissions, &requested_rule, tool.mcp_info.as_ref())
    {
        let can_sandbox_auto_allow = tool.name
            == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME
            && crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled()
            && crate::utils::sandbox::sandbox_adapter::is_auto_allow_bash_if_sandboxed_enabled(
                &crate::utils::settings::get_initial_settings(),
            )
            && crate::tools::bash_tool::should_use_sandbox::should_use_sandbox(
                &crate::tools::bash_tool::should_use_sandbox::SandboxInput {
                    command: input
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    dangerously_disable_sandbox: input
                        .get("dangerouslyDisableSandbox")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
            );
        if !can_sandbox_auto_allow {
            return Some(crate::types::permissions::PermissionDecision::Ask {
                message: create_permission_request_message(&tool.name, None),
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::Rule { rule }),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            });
        }
    }
    let implementation = crate::services::tools::tool_execution::find_tool_call(&tool.name)?;
    let parsed = if let Some(schema) = tool.input_zod_schema.as_ref() {
        crate::utils::zod::safe_parse(schema.0, input).ok()
    } else {
        parse_tool_permission_input(implementation, &tool.name, input)
    }?;
    let result = implementation.check_permissions(&parsed, context);
    if matches!(&result, PermissionResult::Deny { .. })
        || matches!(&result, PermissionResult::Ask { decision_reason, .. }
            if tool_permission_ask_is_bypass_immune(decision_reason.as_ref()))
    {
        return crate::types::permissions::PermissionDecision::try_from(result).ok();
    }
    None
}

/// Maps to CC `permissions.ts:1477-1486#getUpdatedInputOrFallback`.
fn get_updated_input_or_fallback(
    result: &PermissionResult,
    fallback: &serde_json::Value,
) -> serde_json::Value {
    match result {
        PermissionResult::Allow { updated_input, .. }
        | PermissionResult::Ask { updated_input, .. } => {
            updated_input.as_ref().unwrap_or(fallback).clone()
        }
        _ => fallback.clone(),
    }
}

/// Maps to: CC `utils/permissions/permissions.ts:1158-1319#hasPermissionsToUseToolInner`.
/// `PreClassifierOutcome` remains the existing Rust sync/async classifier-await carrier.
///
/// The tool's own `check_permissions` is authoritative here, exactly as in CC —
/// including the trait default's `allow` (CC `Tool.ts:757-769` `TOOL_DEFAULTS`).
/// That makes every tool without an override auto-allowed, which is only sound
/// because the same holds tool-for-tool in CC. Audited against
/// `../rebuild/src/tools/` (the 22 files defining `checkPermissions` there, plus
/// the dynamic MCP tool in `services/mcp/client.ts:1814`):
///
/// - Rust tools WITH an override — Agent, AskUserQuestion, Bash, Config,
///   ExitPlanMode, FileEdit, FileRead, FileWrite, Glob, Grep, LSP, MCP,
///   NotebookEdit, SendMessage, Skill, StructuredOutput, TodoWrite, WebFetch,
///   WebSearch — all have a CC counterpart that defines `checkPermissions`.
/// - Rust tools WITHOUT an override — TaskOutput, TaskStop, TaskCreate, TaskGet,
///   TaskUpdate, TaskList, TeamCreate, TeamDelete, CronCreate, CronDelete,
///   CronList, RemoteTrigger, ToolSearch, Brief/SendUserMessage,
///   ListMcpResourcesTool, ReadMcpResourceTool, EnterPlanMode, EnterWorktree,
///   ExitWorktree — none of their CC counterparts defines `checkPermissions`
///   either, so CC also resolves them through `TOOL_DEFAULTS` to `allow`.
/// - PowerShell's canonical ToolCall::check_permissions mirrors
///   `PowerShellTool.tsx:517-521` and reaches this same generic path.
/// - Dynamic `mcp__*` tools do not resolve in the registry at all and land in the
///   passthrough tail below, matching CC's dynamic tool `checkPermissions`
///   returning `passthrough` (`services/mcp/client.ts:1814-1817`).
fn has_permissions_to_use_tool_inner<'a>(
    params: HasPermissionsToUseToolParams<'a>,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> PreClassifierOutcome<'a> {
    // CC `permissions.ts:1158-1166` opens with the abort check and goes straight
    // into deny/ask rule evaluation. It has no queued/running gate, and neither
    // does `hasPermissionsToUseTool`'s signature
    // (`tool, input, context, assistantMessage, toolUseID`).
    //
    // Cometix used to return `Allow` here whenever the caller reported the tool
    // use as not-queued, which was read off `AssistantMessageKind::ToolUse.status`
    // — so a row whose status had already advanced would skip deny rules, ask
    // rules, and the classifier entirely. Both real callers computed the flag
    // from the same row status this batch removes; every other caller passed a
    // literal `true`, i.e. the branch existed only to be avoided.
    let rule_content =
        permission_rule_content_for_tool(params.tool_name, params.input, params.input_summary);
    let mut request = mock_permission_request_with_input(
        format!("perm-{}", params.tool_use_id),
        params.tool_use_id.to_string(),
        params.tool_name.to_string(),
        rule_content,
        params.input.clone(),
        params.context.mode,
    );
    request.rule.tool_name = permission_rule_tool_name(params.tool_name).to_string();
    request.mcp_info = params.mcp_info.cloned();
    // Maps to: CC `permissions.ts:1210-1213` — the passthrough seed
    // `{ behavior: 'passthrough', message: createPermissionRequestMessage(tool.name) }`
    // that stands until the tool's own `checkPermissions` overwrites it.
    request.message = create_permission_request_message(params.tool_name, None);

    use crate::utils::permissions::permission_result::PermissionResult;
    let process_tool_permission_result = |params: HasPermissionsToUseToolParams<'a>,
                                          result: PermissionResult|
     -> PreClassifierOutcome<'a> {
        let ask_is_bypass_immune = matches!(
            &result,
            PermissionResult::Ask {
                decision_reason,
                ..
            } if tool_permission_ask_is_bypass_immune(decision_reason.as_ref())
        );
        // CC steps 2a/2b run after the early deny/immune-ask returns, including
        // when the tool itself allowed. Re-read the live state after its check.
        if !matches!(&result, PermissionResult::Deny { .. }) && !ask_is_bypass_immune {
            let state = tool_use_context.and_then(crate::tool::ToolUseContext::get_app_state);
            let permissions = state
                .as_ref()
                .map(|s| s.tool_permission_context.as_ref())
                .unwrap_or(params.context);
            let reason = if permissions.mode == PermissionMode::BypassPermissions
                || (permissions.mode == PermissionMode::Plan
                    && permissions.is_bypass_permissions_mode_available)
            {
                Some(PermissionDecisionReason::Mode {
                    mode: permissions.mode,
                })
            } else {
                tool_always_allowed_rule(permissions, &request.rule, request.mcp_info.as_ref())
                    .map(|rule| PermissionDecisionReason::Rule { rule })
            };
            if let Some(decision_reason) = reason {
                let updated_input = get_updated_input_or_fallback(&result, params.input);
                return PreClassifierOutcome::Done {
                    params,
                    result: HasPermissionsToUseToolResult::Allow {
                        updated_input: Some(updated_input),
                        user_modified: None,
                        decision_reason: Some(decision_reason),
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    },
                };
            }
        }
        // CC step 3 (`permissions.ts:1297-1305`) rewrites ONLY `passthrough`'s
        // message with `createPermissionRequestMessage(tool.name, decisionReason)`;
        // a real `ask` keeps the tool's own message.
        let is_passthrough = matches!(&result, PermissionResult::Passthrough { .. });
        // CC `permissions.ts:1302-1308` spreads the tool result into the `ask`,
        // so `metadata` survives step 3 — but only the `ask` arm has it
        // (`types/permissions.ts:208`; the passthrough variant at `:255-266`
        // declares no `metadata`), which is why this is read here instead of in
        // the shared or-pattern below.
        let original_decision =
            crate::types::permissions::PermissionDecision::try_from(result.clone()).ok();
        let pending_classifier_check = match &result {
            PermissionResult::Ask {
                pending_classifier_check,
                ..
            }
            | PermissionResult::Passthrough {
                pending_classifier_check,
                ..
            } => pending_classifier_check.clone(),
            _ => None,
        };
        let metadata = match &result {
            PermissionResult::Ask { metadata, .. } => metadata.clone(),
            _ => None,
        };
        match result {
            // CC `permissions.ts:486-500` returns the tool's allow AS the
            // gate's result, so its payload travels — this is the arm the unit
            // variant used to flatten. #170: the projection is now field-for-
            // field (no `..` drop); a tool's `check_permissions` never sets
            // `toolUseID`/`acceptFeedback`/`contentBlocks` today, but CC's
            // `return result` carries whatever the result holds.
            PermissionResult::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            } => PreClassifierOutcome::Done {
                params,
                result: HasPermissionsToUseToolResult::Allow {
                    updated_input,
                    user_modified,
                    decision_reason,
                    tool_use_id,
                    accept_feedback,
                    content_blocks,
                },
            },
            PermissionResult::Deny {
                decision_reason,
                message,
                ..
            } => {
                // CC `permissions.ts:1224-1226` returns the tool's decision
                // untouched, so the tool's own `message` is what
                // `toolExecution.ts:1023` hands the model.
                let mut request = permission_request_from_tool_result(
                    &params,
                    None,
                    false,
                    Some(decision_reason),
                    message,
                );
                request.permission_result = original_decision;
                PreClassifierOutcome::Done {
                    params,
                    result: HasPermissionsToUseToolResult::Deny(request),
                }
            }
            PermissionResult::Ask {
                suggestions,
                decision_reason,
                message,
                blocked_path,
                ..
            }
            | PermissionResult::Passthrough {
                suggestions,
                decision_reason,
                message,
                blocked_path,
                ..
            } => {
                let is_compound_command = matches!(
                    &decision_reason,
                    Some(PermissionDecisionReason::SubcommandResults { .. })
                );
                // Maps to CC `permissions.ts:1299-1310`: the step-3
                // `passthrough → ask` conversion spreads the tool result, so an
                // `ask` and a converted `passthrough` both keep whatever
                // `decisionReason` the tool produced — including `undefined`.
                let message = if is_passthrough {
                    create_permission_request_message(params.tool_name, decision_reason.as_ref())
                } else {
                    message
                };
                let mut request = permission_request_from_tool_result(
                    &params,
                    Some(&suggestions),
                    is_compound_command,
                    decision_reason,
                    message,
                );
                // The other two fields CC's step-3 spread carries onto the ask
                // that becomes `toolUseConfirm.permissionResult`:
                // `blockedPath` (`BashTool/pathValidation.ts:690`, read by
                // `interactiveHandler.ts:252` and `cli/structuredIO.ts:596`) and
                // `metadata` (`SkillTool.ts:576`, read by
                // `SkillPermissionRequest.tsx:47-52`).
                request.blocked_path = blocked_path;
                request.metadata = metadata;
                request.permission_result = original_decision.or_else(|| {
                    Some(crate::types::permissions::PermissionDecision::Ask {
                        message: request.message.clone(),
                        updated_input: None,
                        decision_reason: request.decision_reason.clone(),
                        suggestions: request.suggestions.clone(),
                        blocked_path: request.blocked_path.clone(),
                        metadata: request.metadata.clone(),
                        is_bash_security_check_for_misparsing: false,
                        pending_classifier_check,
                        content_blocks: Vec::new(),
                    })
                });
                // CC steps 1f (`permissions.ts:1244-1250`) and 1g (`:1255-1260`)
                // RETURN this ask from `hasPermissionsToUseToolInner`, so step 2a's
                // bypassPermissions allow (`:1272-1281`) and step 2b's whole-tool
                // allow (`:1288-1297`) never see it. That immunity is the entire
                // purpose of the two steps, and it is preserved by returning here,
                // above both allow fast-paths.
                //
                // What it is NOT is a final decision. CC's OUTER
                // `hasPermissionsToUseTool` still runs `if (result.behavior ===
                // 'ask')` on this exact value at `:505` — the dontAsk transform at
                // `:508-517` and then the auto branch at `:519+`. Cometix used to
                // short-circuit to `Ask` here, which also skipped the auto branch,
                // so a content ask rule and a `classifierApprovable` safetyCheck
                // never reached the classifier; `:526-531` says both must
                // ("classifierApprovable safetyChecks (sensitive-file paths) fall
                // through to the classifier").
                if ask_is_bypass_immune {
                    return PreClassifierOutcome::Done {
                        params,
                        result: HasPermissionsToUseToolResult::Ask(request),
                    };
                }
                // CC step 3 (`:1299-1310`) returns the ask from the inner; the
                // outer entry's `ask` branch (`:505-551`) owns everything after it.
                PreClassifierOutcome::Done {
                    params,
                    result: HasPermissionsToUseToolResult::Ask(request),
                }
            }
        }
    };

    if params.tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME {
        // Maps to CC `hasPermissionsToUseToolInner(...)` steps 1a-1c:
        // tool-wide deny/ask rules run before `BashTool.checkPermissions(...)`.
        if let Some(rule) =
            get_deny_rule_for_tool(params.context, &request.rule, request.mcp_info.as_ref())
        {
            let message = tool_denied_message(params.tool_name);
            return PreClassifierOutcome::Done {
                params,
                result: HasPermissionsToUseToolResult::Deny(with_rule_decision_reason(
                    request, rule, message,
                )),
            };
        }
        if let Some(rule) =
            get_ask_rule_for_tool(params.context, &request.rule, request.mcp_info.as_ref())
        {
            // Maps to: CC `permissions.ts:1185-1206` — "When
            // autoAllowBashIfSandboxed is on, sandboxed commands skip the ask
            // rule and auto-allow via Bash's checkPermissions. Commands that
            // won't be sandboxed (excluded commands, dangerouslyDisableSandbox)
            // still need to respect the ask rule." All four conditions of
            // `canSandboxAutoAllow` (`:1189-1193`); `should_use_sandbox`
            // re-checks `isSandboxingEnabled` internally, matching CC's
            // redundant pair.
            let settings = crate::utils::settings::get_initial_settings();
            let can_sandbox_auto_allow =
                crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled()
                    && crate::utils::sandbox::sandbox_adapter::is_auto_allow_bash_if_sandboxed_enabled(
                        &settings,
                    )
                    && crate::tools::bash_tool::should_use_sandbox::should_use_sandbox(
                        &crate::tools::bash_tool::should_use_sandbox::SandboxInput {
                            command: params
                                .input
                                .get("command")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            dangerously_disable_sandbox: params
                                .input
                                .get("dangerouslyDisableSandbox")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        },
                    );
            if !can_sandbox_auto_allow {
                let message = create_permission_request_message(params.tool_name, None);
                return PreClassifierOutcome::Done {
                    params,
                    result: HasPermissionsToUseToolResult::Ask(with_rule_decision_reason(
                        request, rule, message,
                    )),
                };
            }
            // CC `:1205`: fall through to let Bash's checkPermissions handle
            // command-specific rules.
        }
        let fallback_context;
        let tool_context = if let Some(context) = tool_use_context {
            context
        } else {
            fallback_context =
                crate::tool::ToolUseContext::with_permission_context(params.context.clone());
            &fallback_context
        };
        let result = crate::tool::ToolCall::check_permissions(
            &crate::tools::bash_tool::BashTool,
            params.input,
            tool_context,
        );
        return process_tool_permission_result(params, result);
    }

    // CC's outer permission owner inspects only tool-wide rules here. Read
    // content/path rules remain inside checkReadPermissionForTool so its UNC,
    // suspicious-path, deny, ask, edit-implies-read, working/internal, and
    // allow ordering cannot be preempted by a generic string matcher.
    if let Some(rule) =
        get_deny_rule_for_tool(params.context, &request.rule, request.mcp_info.as_ref())
    {
        let message = tool_denied_message(params.tool_name);
        return PreClassifierOutcome::Done {
            params,
            result: HasPermissionsToUseToolResult::Deny(with_rule_decision_reason(
                request, rule, message,
            )),
        };
    }
    if let Some(rule) =
        get_ask_rule_for_tool(params.context, &request.rule, request.mcp_info.as_ref())
    {
        let message = create_permission_request_message(params.tool_name, None);
        return PreClassifierOutcome::Done {
            params,
            result: HasPermissionsToUseToolResult::Ask(with_rule_decision_reason(
                request, rule, message,
            )),
        };
    }

    // Maps to CC `inputSchema.parse(input)` immediately before
    // `tool.checkPermissions(...)`. This clone is decision-only: parse failure
    // leaves the Tool result as passthrough and the raw input remains on the
    // request for the eventual call boundary.
    if let Some(tool_call) =
        crate::services::tools::tool_execution::find_tool_call(params.tool_name)
    {
        let permission_input =
            parse_tool_permission_input(tool_call, params.tool_name, params.input);
        if let Some(permission_input) = permission_input {
            let fallback_context;
            let tool_context = if let Some(context) = tool_use_context {
                context
            } else {
                // Context-free callers (standalone permission helpers/tests) retain
                // the default process cwd; production orchestration passes the
                // exact ToolUseContext through the full-context entry above.
                fallback_context =
                    crate::tool::ToolUseContext::with_permission_context(params.context.clone());
                &fallback_context
            };
            let tool_permission = tool_call.check_permissions(&permission_input, tool_context);
            if tool_call.requires_user_interaction() {
                if let crate::utils::permissions::permission_result::PermissionResult::Ask {
                    suggestions,
                    decision_reason,
                    message,
                    ..
                } = &tool_permission
                {
                    // CC step 1e returns this ask before bypass and whole-tool
                    // allow checks. dontAsk remains the final outer transform.
                    let mut request = permission_request_from_tool_result(
                        &params,
                        Some(suggestions),
                        matches!(
                            decision_reason,
                            Some(PermissionDecisionReason::SubcommandResults { .. })
                        ),
                        decision_reason.clone(),
                        message.clone(),
                    );
                    request.permission_result =
                        crate::types::permissions::PermissionDecision::try_from(
                            tool_permission.clone(),
                        )
                        .ok();
                    let result = HasPermissionsToUseToolResult::Ask(request);
                    return PreClassifierOutcome::Done { params, result };
                }
            }
            // The tool's own result IS the decision. CC step 3
            // (permissions.ts:1299-1310) rewrites ONLY `passthrough` into `ask`;
            // `allow` and `ask` are returned verbatim, and the outer entry
            // (permissions.ts:486-501) returns an `allow` before the dontAsk
            // transform and the auto-mode classifier ever see it.
            //
            // Cometix used to keep the result only for `ask`/`deny` plus eight
            // hardcoded filesystem/search tools, and re-decided every `allow`
            // against an invented allow-list further down. That discarded
            // AgentTool's `{behavior:'allow'}` (AgentTool.tsx:1692-1708, whose
            // only live external-build branch is the allow) and prompted on
            // every subagent spawn, which CC never does.
            return process_tool_permission_result(params, tool_permission);
        }

        // CC permissions.ts:1218-1224: schema errors retain passthrough;
        // the same mode and whole-tool rule checks below still run.
    }

    // Reached only by names the registry cannot resolve (dynamic `mcp__*` tools,
    // unknown/legacy names) and by registry tools whose input failed the schema
    // check. Both are CC's passthrough seed (permissions.ts:1210-1213), so this
    // tail is CC steps 2a/2b/3 plus the outer dontAsk + classifier transforms.
    //
    // The `tool_has_builtin_allow_permission` / `config_get_allows` /
    // `remote_trigger_read_allows` allow-list that used to sit here had no CC
    // owner: it existed only because the branch above discarded the tool's own
    // `allow`. Every name on it now gets that allow back from its own
    // `check_permissions` (see the audit note above `has_permissions_to_use_tool_inner`).
    process_tool_permission_result(
        params,
        PermissionResult::Passthrough {
            message: request.message.clone(),
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            pending_classifier_check: None,
        },
    )
}

/// Maps to: CC `tool.inputSchema.parse(input)` — the zod parse CC performs
/// immediately before every `tool.checkPermissions(...)` call. CC has two such
/// call sites (`permissions.ts:1215-1216` for the primary decision and
/// `:606-607` for the acceptEdits fast-path overlay), both reading the schema
/// off the same `Tool` object, so this port keeps one owner for the projection
/// rather than transcribing the normalize/validate block twice.
///
/// `None` is CC's `parse` throw, which the caller turns into "the tool's own
/// decision does not run": the primary site leaves `toolPermissionResult` at the
/// passthrough seed, and the acceptEdits site falls through to the classifier
/// via CC's `catch` at `:650-655`.
fn parse_tool_permission_input(
    tool_call: &'static dyn crate::tool::ToolCall,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<serde_json::Value> {
    let decision_input = input.clone();
    let normalized_input = tool_call.normalize_input(&decision_input);
    // Native Zod carrier (#144): CC's `tool.inputSchema.parse(input)` returns
    // the PARSE RESULT — Agent's plain `z.object` accepts and STRIPS unknown
    // fields. The JSON-Schema fallback below cannot reproduce that: the
    // projection carries `additionalProperties: false` for both object kinds
    // (zod/v4 oracle), so validating against it rejects what CC strips.
    if tool_call.name() == crate::tools::agent_tool::constants::AGENT_TOOL_NAME {
        return crate::utils::zod::safe_parse(
            crate::tools::agent_tool::input_schema(),
            &normalized_input,
        )
        .ok();
    }
    let definitions = crate::tools::get_all_base_tools();
    match crate::types::tools::find_tool_by_name(&definitions, tool_name)
        .or_else(|| crate::types::tools::find_tool_by_name(&definitions, tool_call.name()))
    {
        // Validation failure is CC's `tool.inputSchema.parse(input)` throw.
        Some(definition) => jsonschema::draft7::new(&definition.input_schema)
            .ok()
            .and_then(|validator| {
                validator
                    .is_valid(&normalized_input)
                    .then_some(normalized_input)
            }),
        // No schema row is NOT a parse failure. CC reads the schema off the
        // Tool object itself, so `tool.checkPermissions` always runs
        // (permissions.ts:1215-1216). Cometix keeps schemas in the
        // gate-filtered `get_all_base_tools()` table instead, so a registered
        // behavior can have no row — always for StructuredOutput and `mcp`,
        // and for every feature-gated tool while its gate is off. Suppressing
        // the tool's own decision there is a Cometix representation gap.
        None => Some(normalized_input),
    }
}

/// Maps to: CC `permissions.ts:1170-1181` (step 1a deny) and `:1184-1203`
/// (step 1b ask) — both decisions carry the rule that actually MATCHED as
/// `decisionReason: { type: 'rule', rule }`.
///
/// `request.rule` is the requested rule value (built from this input), so it is
/// never the answer here: quoting it back is how the port ended up telling the
/// user "Permission rule Agent({...whole json...}) requires confirmation" for a
/// decision where no rule existed at all.
///
/// Both CC decisions also carry a `message`: `` `Permission to use ${tool.name}
/// has been denied.` `` for the deny (`:1179`) and
/// `createPermissionRequestMessage(tool.name)` for the ask (`:1197`).
fn with_rule_decision_reason(
    mut request: PermissionRequest,
    rule: PermissionRule,
    message: String,
) -> PermissionRequest {
    request.decision_reason = Some(PermissionDecisionReason::Rule { rule });
    request.message = message;
    request
}

/// Maps to: CC `permissions.ts:1179` — the step-1a whole-tool deny message.
fn tool_denied_message(tool_name: &str) -> String {
    format!("Permission to use {tool_name} has been denied.")
}

/// Maps to: CC `permissions.ts:509-516` — the dontAsk ask→deny transform. CC
/// builds a WHOLE new decision there rather than spreading the ask, so both the
/// `message` (`DONT_ASK_REJECT_MESSAGE(tool.name)`) and the `decisionReason`
/// (`{ type: 'mode', mode: 'dontAsk' }`) are replaced; the originating ask's own
/// reason does not survive.
///
/// The reason is consumed rather than decorative: `serializeDecisionReason`
/// (`cli/structuredIO.ts:64-91`) routes on `reason.type`, and
/// `decisionReasonToOTelSource` (`toolExecution.ts:952-977`) attributes the
/// denial by it. `PermissionRuleExplanation` is NOT a consumer — its
/// `stringsForDecisionReason` falls through `mode` to `default: return null`
/// (`PermissionRuleExplanation.tsx:48-83`) — and a deny never reaches a dialog
/// anyway.
fn with_dont_ask_deny_message(
    mut request: PermissionRequest,
    tool_name: &str,
) -> PermissionRequest {
    request.message = crate::utils::messages::dont_ask_reject_message(tool_name);
    request.decision_reason = Some(PermissionDecisionReason::Mode {
        mode: PermissionMode::DontAsk,
    });
    request
}

/// Maps to: CC `utils/permissions/permissions.ts:505-551` — the `ask` branch of
/// the OUTER `hasPermissionsToUseTool`, which runs on whatever
/// `hasPermissionsToUseToolInner` returned at `:480`. That includes the
/// bypass-immune asks steps 1f/1g return early (`:1244-1260`): being immune to
/// step 2a's allow does not exempt them from this branch.
///
/// The port has exactly one owner for this branch, so every producer of an ask
/// inside `has_permissions_to_use_tool_inner` — the step-1b whole-tool ask rule,
/// the step-1f/1g immune asks, and the step-3 `passthrough → ask` conversion —
/// funnels through here, as all of them do in CC by simply returning.
fn maybe_classifier_for_ask(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
) -> PreClassifierOutcome<'_> {
    // CC `:508-517` — dontAsk turns the ask into a deny with
    // `DONT_ASK_REJECT_MESSAGE(tool.name)`, ahead of the auto branch.
    if params.context.mode == PermissionMode::DontAsk {
        let request = with_dont_ask_deny_message(request, params.tool_name);
        return PreClassifierOutcome::Done {
            params,
            result: HasPermissionsToUseToolResult::Deny(request),
        };
    }
    // CC `:520-525` — `feature('TRANSCRIPT_CLASSIFIER') && (mode === 'auto' ||
    // (mode === 'plan' && isAutoModeActive()))`.
    if should_try_auto_mode_classifier(params.context) {
        // CC `:532-548`, whose own comment (`:526-531`) states the split:
        // "Non-classifier-approvable safetyCheck decisions stay immune to ALL
        // auto-approve paths: the acceptEdits fast-path, the safe-tool allowlist,
        // and the classifier. Step 1g only guards bypassPermissions; this guards
        // auto. classifierApprovable safetyChecks (sensitive-file paths) fall
        // through to the classifier".
        //
        // So the `classifierApprovable` flag is read HERE and only here — step 1g
        // (`:1255-1260`) deliberately does not look at it.
        if matches!(
            request.decision_reason,
            Some(PermissionDecisionReason::SafetyCheck {
                classifier_approvable: false,
                ..
            })
        ) {
            if params.context.should_avoid_permission_prompts {
                // CC `:536-546`. `message` stays `result.message` — the ask's own
                // text — and only `decisionReason` is replaced.
                let mut deny_request = request;
                deny_request.decision_reason = Some(PermissionDecisionReason::AsyncAgent {
                    reason: "Safety check requires interactive approval and permission prompts are not available in this context".to_string(),
                });
                return PreClassifierOutcome::Done {
                    params,
                    result: HasPermissionsToUseToolResult::Deny(deny_request),
                };
            }
            // CC `:547` returns the untouched ask.
            return PreClassifierOutcome::Done {
                params,
                result: HasPermissionsToUseToolResult::Ask(request),
            };
        }
        // CC `:549-551` `tool.requiresUserInteraction?.() && result.behavior === 'ask'`.
        if crate::services::tools::tool_execution::find_tool_call(params.tool_name)
            .is_some_and(crate::tool::ToolCall::requires_user_interaction)
        {
            return PreClassifierOutcome::Done {
                params,
                result: HasPermissionsToUseToolResult::Ask(request),
            };
        }
        return PreClassifierOutcome::NeedsClassifier { params, request };
    }
    // CC `:929-952` — the non-auto tail. Every auto-branch path above RETURNS,
    // so this runs only when the ask fell past the whole auto block: prompts
    // unavailable → hooks get one chance, then auto-deny. The hook await lives
    // in the outer entries (sync bridge / async twin).
    if params.context.should_avoid_permission_prompts {
        return PreClassifierOutcome::NeedsHeadlessHooks { params, request };
    }
    // CC falls past the whole auto block and returns the ask at `:955`.
    PreClassifierOutcome::Done {
        params,
        result: HasPermissionsToUseToolResult::Ask(request),
    }
}

/// Maps to: CC `permissions.ts:400-471#runPermissionRequestHooksForHeadlessAgent`
/// plus its ONLY caller, the non-auto ask tail (`:929-952`).
///
/// CC iterates the PermissionRequest hook results and the FIRST allow/deny
/// decision wins (`:409-460`); with no decision (or no hooks, or a hook
/// failure) the ask auto-denies with `{type:'asyncAgent', reason:'Permission
/// prompts are not available in this context'}` and
/// `AUTO_REJECT_MESSAGE(tool.name)` (`:944-951`).
///
/// A hook allow both PERSISTS its `updatedPermissions` and projects them into
/// the live AppState (`:425-433`), in that order — a `persistPermissionUpdates`
/// throw takes the whole decision to the catch, so the port applies the
/// projection only after a successful persist (same fail-closed rule as the
/// interactive `PermissionContext.ts:139-147#persistPermissions`). A hook deny
/// with `interrupt` aborts the controller before returning (`:445-450`).
async fn resolve_headless_ask_async(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    resolve_headless_ask_owned(
        params.input.clone(),
        request,
        tool_use_context.map(|context| context.abort_controller.clone()),
        // CC hands `runPermissionRequestHooksForHeadlessAgent` the whole
        // `context` (`permissions.ts:933-940`, forwarded at `:409-417`) and
        // `executeHooks` reads exactly one thing off it for the hook SET:
        // `toolUseContext?.agentId ?? getSessionId()` (`hooks.ts:2003`). That
        // projection is what crosses into the owned body.
        tool_use_context.and_then(|context| context.agent_id.clone()),
        tool_use_context
            .map(|context| context.app_store.clone())
            .unwrap_or_default(),
    )
    .await
}

/// The body of [`resolve_headless_ask_async`], over OWNED inputs only.
///
/// The things this tail actually reads off the caller are the tool input (the
/// allow arm's `updatedInput` fallback), the abort controller the hooks observe
/// and the deny arm aborts, the agent id that keys the hook set, and the store
/// handles the allow arm's `setAppState` writes through. Taking them by value
/// makes the future `'static`, which is what lets [`resolve_headless_ask_sync`]
/// hand it to a scratch thread when the ambient runtime is `current_thread`.
///
/// Hook SET (`permissions.ts:409-417` → `hooks.ts:4182-4191#executeHooks` →
/// `:2001-2010#getMatchingHooks` → `:1491-1565#getHooksConfig`): CC assembles
/// FOUR sources, in this order — the settings snapshot
/// (`getHooksConfigFromSnapshot()?.[hookEvent]`), the registered SDK/plugin
/// hooks (`getRegisteredHooks()`, plugin entries filtered out under
/// `shouldAllowManagedHooksOnly()`), then, unless managed-only, the current
/// session's `appState.sessionHooks` and its function hooks. The session id is
/// `toolUseContext?.agentId ?? getSessionId()` (`:2003`), which on THIS path is
/// the background/headless agent's own id.
///
/// `load_hooks_config()` covers only the first two ("Session-derived hooks
/// remain scoped and merged by their callers", its own doc comment), so this
/// tail used to run none of the hooks an agent registers at runtime from its own
/// frontmatter (CC `runAgent.ts:568` `registerFrontmatterHooks`, ported as
/// `run_agent.rs#register_agent_frontmatter_hooks_for_run`, which keys them by
/// `agentId`) — the agent's declared permission policy never ran on the one
/// permission path that agent actually uses. The merge is the shared
/// `session_hooks::merge_session_hooks_into_config` under its
/// `allow_managed_hooks_only` gate (CC `:1515` + `:1541`), as in
/// `tool_execution.rs#load_tool_hooks_config_and_env`,
/// `run_agent.rs#subagent_hook_config_and_env`, `process_user_input/mod.rs` and
/// `cli/print.rs#execute_permission_request_hooks_for_sdk`. Session FUNCTION
/// hooks stay out: `merge_session_hooks_into_config` flattens command hooks
/// only, and this port has no `FunctionHookMatcher` executor yet.
async fn resolve_headless_ask_owned(
    input: serde_json::Value,
    mut request: PermissionRequest,
    abort_controller: Option<crate::tool::AbortController>,
    agent_id: Option<String>,
    app_store: crate::tool::AppStoreRef,
) -> HasPermissionsToUseToolResult {
    let loaded = crate::services::hooks::load_hooks_config();
    let mut config = loaded.config;
    // Maps to: CC `hooks.ts:1534-1541` — `getHooksConfig` gates its session arm
    // on `managedOnly` (`:1516`, `shouldAllowManagedHooksOnly()`) and on nothing
    // else. `shouldDisableAllHooksIncludingManaged()` belongs to a DIFFERENT
    // layer: CC evaluates it once at execution entry (`:1978-1980`, the first
    // statement of `executeHooks`), never at merge time.
    //
    // The port now matches, because it now has that execution layer. When
    // a772a9d added `!loaded.disable_all_hooks` here, `services/hooks/tool.rs`
    // "carrie[d] no such guard" — its own words — and `load_hooks_config()`
    // applies the policy only to the two sources it owns (empty snapshot via
    // `get_hooks_from_allowed_sources`, `mod.rs:184` for the registered arm), so
    // this merge site was the only thing standing between an enterprise
    // `disableAllHooks` and an agent's own permission hooks. 8bb2e62 then routed
    // every executor through `should_skip_hook_execution`, and this tail reaches
    // it: `execute_permission_request_hooks` (`permission_request.rs:88`)
    // delegates to `tool::execute_hooks`, which opens with the gate
    // (`tool.rs:169`) ahead of `get_matching_hooks`. The conjunct had become a
    // SECOND copy of an admin control, and duplicated admin controls are what
    // drift apart — so it is gone, and which layer stops the hook is pinned by
    // `headless_ask_skips_session_hooks_under_a_disable_all_hooks_policy`.
    //
    // The managed-ONLY conjunct stays, and has no execution-layer twin to be
    // redundant with: in CC it is a per-source merge FILTER (`:1516`, `:1541`),
    // not a per-call gate — the distinction
    // `untrusted_workspace_stops_every_executor_family_without_filtering`
    // documents from the other side. Its own layer is pinned by
    // `headless_ask_skips_session_hooks_under_an_allow_managed_hooks_only_policy`.
    if !loaded.allow_managed_hooks_only {
        // CC never skips the lookup when there is no agentId — `?? getSessionId()`
        // is the main-session arm, and it is what a `/hooks`-registered or
        // skill-registered PermissionRequest hook lives under.
        let session_id = agent_id.unwrap_or_else(crate::bootstrap::state::get_session_id);
        crate::utils::hooks::session_hooks::merge_session_hooks_into_config(
            &mut config,
            &session_id,
        );
    }
    // CC has no early return here; `getMatchingHooks` simply yields nothing and
    // the generator completes. The port keeps the cheap guard, but it has to be
    // read off the MERGED set or a session-only hook is skipped before it runs.
    let has_hooks = config
        .get("PermissionRequest")
        .is_some_and(|entries| !entries.is_empty());
    if has_hooks {
        let results = crate::services::hooks::permission_request::execute_permission_request_hooks(
            &config,
            &request,
            Vec::new(),
            abort_controller.as_ref(),
        )
        .await;
        for result in &results {
            // CC `:409-412` skips on `!hookResult.permissionRequestResult` and
            // then switches on `decision.behavior` — NOT on the flattened
            // `permissionBehavior`, which a top-level `{"decision":"approve"}`
            // also sets. Reading the flattened field let such a hook allow a
            // headless ask that CC leaves to the auto-deny.
            let Some(decision) = result.permission_request_result.as_ref() else {
                continue;
            };
            match decision {
                crate::types::hooks::PermissionRequestResult::Allow {
                    updated_input,
                    updated_permissions,
                } => {
                    // CC `:422-443` — allow with the hook's updatedInput
                    // (falling back to the original) and the hook reason.
                    //
                    // Persist-vs-state ordering (#170): THIS path's CC anchor
                    // is `permissions.ts:426-433` — `persistPermissionUpdates`
                    // FIRST (`:426`), `setAppState` after (`:427-433`); the
                    // #159 fail-closed rule (project only after a successful
                    // persist) restates it. The SDK `can_use_tool` normalizer
                    // follows the OPPOSITE order from its own anchor,
                    // `PermissionPromptToolResultSchema.ts:98-105` — see
                    // `cli/print.rs#normalized_sdk_permission_response`. The
                    // two are deliberately not unified.
                    if !updated_permissions.is_empty() {
                        match crate::utils::permissions::permission_update::persist_permission_updates(
                            updated_permissions,
                        ) {
                            // CC `:428-433` — the SAME updates are projected
                            // into the live AppState, read off `prev` rather
                            // than off any snapshot this tail carries.
                            Ok(()) => app_store.set_app_state(|state| {
                                state.tool_permission_context = std::sync::Arc::new(
                                    crate::utils::permissions::permission_update::apply_permission_updates(
                                        &state.tool_permission_context,
                                        updated_permissions,
                                    ),
                                );
                            }),
                            // CC's `persistPermissionUpdates` throw would take
                            // the whole decision to `:463-469`'s catch. The
                            // port keeps its narrower log-and-continue (booked
                            // deviation) but must not project what it failed
                            // to persist — same fail-closed order as
                            // `PermissionContext.ts:141-145`.
                            Err(error) => crate::utils::debug::log_for_debugging(&format!(
                                "Failed to persist PermissionRequest hook updates: {error}"
                            )),
                        }
                    }
                    return HasPermissionsToUseToolResult::Allow {
                        updated_input: Some(updated_input.clone().unwrap_or_else(|| input.clone())),
                        user_modified: None,
                        decision_reason: Some(PermissionDecisionReason::Hook {
                            hook_name: "PermissionRequest".to_string(),
                            hook_source: None,
                            reason: None,
                        }),
                        // CC's hook-allow literal (`permissions.ts:435-442`)
                        // sets none of the remaining allow fields.
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    };
                }
                crate::types::hooks::PermissionRequestResult::Deny { message, interrupt } => {
                    // CC `:445-450` — a truthy `interrupt` aborts the turn's
                    // controller BEFORE the deny is returned. The producer here
                    // is the hook, not a model-authored permission-prompt tool
                    // result, and the bit reaches this arm only after
                    // `hook_json_output_schema`'s `z.boolean().optional()`.
                    if *interrupt == Some(true) {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Hook interrupt: tool={} hookMessage={}",
                            request.tool_name,
                            message.clone().unwrap_or_default()
                        ));
                        if let Some(abort_controller) = abort_controller.as_ref() {
                            abort_controller.abort();
                        }
                    }
                    // CC `:451-458` — `decision.message || 'Permission denied
                    // by hook'`, with the message doubling as the hook reason.
                    // It comes off the decision object; `permissionDecisionReason`
                    // belongs to the PreToolUse arm and is stripped out of a
                    // PermissionRequest hook's output by the schema, so reading
                    // it here always produced the fallback copy and a `None`
                    // reason.
                    // `||` is JS-truthy, so an empty message takes the fallback
                    // while `decisionReason.reason` keeps the raw value.
                    request.message = message
                        .clone()
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "Permission denied by hook".to_string());
                    request.decision_reason = Some(PermissionDecisionReason::Hook {
                        hook_name: "PermissionRequest".to_string(),
                        hook_source: None,
                        reason: message.clone(),
                    });
                    return HasPermissionsToUseToolResult::Deny(request);
                }
            }
        }
    }
    // CC `:944-951` — no hook decision: auto-deny.
    request.message = crate::utils::messages::auto_reject_message(&request.tool_name);
    request.decision_reason = Some(PermissionDecisionReason::AsyncAgent {
        reason: "Permission prompts are not available in this context".to_string(),
    });
    HasPermissionsToUseToolResult::Deny(request)
}

/// Sync bridge over [`resolve_headless_ask_async`]: CC's path is fully async
/// and this shim only exists because `has_permissions_to_use_tool` is still
/// sync.
///
/// The bridge is [`crate::utils::process_runtime::block_on_from_sync`] rather
/// than a hand-rolled `block_in_place(|| handle.block_on(…))`, which is a
/// PANIC here: every query actor runs on a `current_thread` runtime
/// (`query.rs#spawn_query`), and `block_in_place` requires the multi-threaded
/// one. Any subagent reaching a headless ask through
/// `tool_execution.rs:371/438/853` would have hit it.
fn resolve_headless_ask_sync(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    // The future must be `'static` to cross to a scratch thread, so the values
    // this tail reads off the caller are taken by value first. `AppStoreRef` is
    // a handle: the clone still writes through to the same live store.
    let input = params.input.clone();
    let abort_controller = tool_use_context.map(|context| context.abort_controller.clone());
    // Same projection as the async twin: CC `hooks.ts:2003`
    // `toolUseContext?.agentId ?? getSessionId()` keys the hook set.
    let agent_id = tool_use_context.and_then(|context| context.agent_id.clone());
    let app_store = tool_use_context
        .map(|context| context.app_store.clone())
        .unwrap_or_default();
    let fallback_request = request.clone();
    let resolved = crate::utils::process_runtime::block_on_from_sync(resolve_headless_ask_owned(
        input,
        request,
        abort_controller,
        agent_id,
        app_store,
    ));
    match resolved {
        Some(result) => result,
        None => {
            // No runtime at all: the hooks cannot run; CC's no-decision
            // auto-deny is the only sound outcome.
            let mut request = fallback_request;
            request.message = crate::utils::messages::auto_reject_message(&request.tool_name);
            request.decision_reason = Some(PermissionDecisionReason::AsyncAgent {
                reason: "Permission prompts are not available in this context".to_string(),
            });
            HasPermissionsToUseToolResult::Deny(request)
        }
    }
}

/// Maps to CC `mode === 'auto' || (mode === 'plan' && isAutoModeActive())`.
fn should_try_auto_mode_classifier(context: &ToolPermissionContext) -> bool {
    if !crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
        return false;
    }
    context.mode == PermissionMode::Auto
        || (context.mode == PermissionMode::Plan
            && crate::utils::permissions::auto_mode_state::is_auto_mode_active())
}

/// Project the current action through the active Tool's
/// `toAutoClassifierInput`. Maps to CC `yoloClassifier.ts#toCompactBlock`.
/// `None` is the Tool-default empty projection and means no security-relevant
/// classifier action; an unknown dynamic tool retains its raw input.
/// CC's `tools` argument to `classifyYoloAction`, which every CC caller holds on
/// hand (`yoloClassifier.ts:1015`) and turns into a lookup keyed by name and
/// alias (`buildToolLookup:364-372`).
///
/// Rust's permission entry takes `Option<&ToolUseContext>` because some callers
/// have no context at all — skill shell checks, direct tests. `None` yields an
/// empty lookup, which is exactly CC's `buildToolLookup([])`: every tool_use
/// block in the transcript is dropped. That is the conservative reading, and it
/// never invents tools the caller does not actually hold.
fn classifier_tool_lookup(
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> &[crate::types::tools::Tool] {
    tool_use_context.map_or(&[], |context| context.tools.as_slice())
}

fn projected_classifier_input(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<serde_json::Value> {
    let Some(tool) = crate::services::tools::tool_execution::find_tool_call(tool_name) else {
        return Some(input.clone());
    };
    let projected = tool.to_auto_classifier_input(input);
    (!projected.is_empty()).then(|| serde_json::Value::String(projected))
}

/// Maps to: CC `permissions.ts:555-686` — everything the auto-mode branch does
/// between loading the denial state and calling the classifier.
///
/// `Decided` is a CC `return` that never reaches `classifyYoloAction`;
/// `Classify` is CC falling through to `:689-693`.
enum AutoModePreClassifier {
    Decided(HasPermissionsToUseToolResult),
    Classify(PermissionRequest),
}

/// Maps to: CC `utils/permissions/permissions.ts:572-686` — the PowerShell
/// guard, the acceptEdits fast-path, and the safe-tool allowlist, in that order,
/// immediately before `formatActionForClassifier` / `classifyYoloAction`
/// (`:689-693`).
///
/// Called from both classifier entries because this port splits CC's single
/// `async` owner into a sync bridge and an async twin; the block is not
/// duplicated (same arrangement as `reset_consecutive_denials_on_allow`).
///
/// The two CC guards that sit ABOVE this block both live in
/// `maybe_classifier_for_ask`, which is the sole producer of
/// `PreClassifierOutcome::NeedsClassifier`, so neither can be skipped:
/// - `:532-548` the non-`classifierApprovable` safetyCheck immunity;
/// - `:549-551` `requiresUserInteraction && ask`.
///
/// The 2026-08-26 re-verification of the #129 "the `:532-548` guard cannot
/// change either fast-path's answer today" argument found it **false**, which is
/// why the guard landed with this fix rather than later. Rust has two
/// `classifier_approvable: false` producers, not one:
/// - `filesystem.rs:525-530` (suspicious Windows path). For this one the #129
///   argument does hold: `filesystem.rs:1279-1288` returns the `Ask` before the
///   acceptEdits allow at `:1314-1320`, so the fast-path below re-derives the
///   same `Ask`, and no write tool is on the allowlist.
/// - `tools/send_message_tool/mod.rs:918-932` (`SendMessageTool.ts:585-602`
///   cross-machine bridge consent). `SendMessage` **is** on the auto-mode
///   allowlist (`classifierDecision.ts:83`, `classifier_decision.rs:34`), so
///   without `:532-548` the allowlist fast-path below would ALLOW exactly the
///   prompt-injection vector CC's `:590-592` comment says must stay immune.
///   It is currently unreachable only because `FeatureFlag::UdsInbox` is
///   `enabled: false`, whose own note promises that "enabling the transport
///   cannot silently skip" this gate.
///
/// SEAM (analytics unported): CC emits `logEvent('tengu_auto_mode_decision', …)`
/// at `:626-640` (acceptEdits fast-path) and `:666-677` (allowlist fast-path),
/// both with `decision: 'allowed'`, `confidence: 'high'` and a `fastPath`
/// discriminator. There is no `log_event` in this tree; the two call sites are
/// marked below so they can be filled in one pass when analytics lands.
fn auto_mode_pre_classifier_fast_path(
    params: &mut HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> AutoModePreClassifier {
    // Maps to: CC `:555-558`
    // `context.localDenialTracking ?? appState.denialTracking ?? createDenialTrackingState()`.
    let denial_state =
        load_denial_tracking(params.app_store, params.local_denial_tracking.as_ref());

    // Maps to: CC `:572-591`. `feature('POWERSHELL_AUTO_MODE')` is CC's own
    // "ant-only build flag" (`:561`), so in the external profile this port
    // targets the guard is live. Porting the guard rather than its outcome
    // keeps the internal profile.
    if params.tool_name == crate::tools::powershell_tool::tool_name::POWERSHELL_TOOL_NAME
        && !crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Permissions,
        )
    {
        if params.context.should_avoid_permission_prompts {
            // CC `:576-586`.
            let mut deny_req = request;
            // CC `:576-586` puts this string on the DECISION's `message`, not on
            // the tool's own `description(input)`.
            deny_req.message = "PowerShell tool requires interactive approval".to_string();
            deny_req.decision_reason = Some(PermissionDecisionReason::AsyncAgent {
                reason: "PowerShell tool requires interactive approval and permission prompts are not available in this context".to_string(),
            });
            return AutoModePreClassifier::Decided(HasPermissionsToUseToolResult::Deny(deny_req));
        }
        // CC `:587-590`: log, then return the untouched ask.
        tracing::debug!(
            tool = params.tool_name,
            "Skipping auto mode classifier for {}: tool requires explicit user permission",
            params.tool_name
        );
        return AutoModePreClassifier::Decided(HasPermissionsToUseToolResult::Ask(request));
    }

    // Maps to: CC `:600-656` — before paying for a classifier call, re-run the
    // tool's own `checkPermissions` under an `acceptEdits` overlay. Agent and
    // REPL are excluded at `:602-603`: their `checkPermissions` returns `allow`
    // in acceptEdits mode, which would silently bypass the classifier.
    //
    // `REPL` is CC `tools/REPLTool/constants.ts:11#REPL_TOOL_NAME`; the tool
    // itself is not ported, so the literal is spelled here rather than given an
    // invented Rust owner.
    if params.tool_name != crate::tools::agent_tool::constants::AGENT_TOOL_NAME
        && params.tool_name != "REPL"
    {
        // CC always holds a `Tool` here. A name the registry cannot resolve
        // (dynamic `mcp__*`, unknown/legacy) has no `checkPermissions` to re-run,
        // which is the same outcome as CC's `catch` at `:650-655`: fall through
        // to the allowlist and then the classifier.
        if let Some(tool_call) =
            crate::services::tools::tool_execution::find_tool_call(params.tool_name)
        {
            // CC `:606` `tool.inputSchema.parse(input)`; a throw is caught at
            // `:650-655` and falls through to the classifier.
            if let Some(parsed_input) =
                parse_tool_permission_input(tool_call, params.tool_name, params.input)
            {
                // CC `:607-619` spreads the context and overrides ONLY
                // `getAppState`, so the overlay is local to this one call and is
                // never written back. `update_permission_context` would commit
                // to the live AppStore, so the field is set directly.
                let fallback_context;
                let base_context = if let Some(context) = tool_use_context {
                    context
                } else {
                    fallback_context = crate::tool::ToolUseContext::with_permission_context(
                        params.context.clone(),
                    );
                    &fallback_context
                };
                let base_get_state = base_context.clone();
                let mut overlay_context = base_context.clone().with_get_app_state_override(
                    crate::tool::GetAppStateCallback::new(move || {
                        let state = base_get_state.get_app_state()?;
                        let mut state = (*state).clone();
                        let mut permission = (*state.tool_permission_context).clone();
                        permission.mode = PermissionMode::AcceptEdits;
                        state.tool_permission_context = std::sync::Arc::new(permission);
                        Some(std::sync::Arc::new(state))
                    }),
                );
                overlay_context.tool_permission_context.mode = PermissionMode::AcceptEdits;
                let accept_edits_result =
                    tool_call.check_permissions(&parsed_input, &overlay_context);
                if let crate::utils::permissions::permission_result::PermissionResult::Allow {
                    updated_input,
                    ..
                } = accept_edits_result
                {
                    // CC `:621-622` recordSuccess + persistDenialState.
                    let new_denial_state =
                        crate::utils::permissions::denial_tracking::record_success(denial_state);
                    persist_denial_tracking(
                        params.app_store,
                        params.local_denial_tracking.as_ref(),
                        new_denial_state,
                    );
                    // CC `:623-625`.
                    tracing::debug!(
                        tool = params.tool_name,
                        "Skipping auto mode classifier for {}: would be allowed in acceptEdits mode",
                        params.tool_name
                    );
                    // SEAM (analytics unported): CC `:626-640`
                    // logEvent('tengu_auto_mode_decision', { fastPath: 'acceptEdits' }).
                    //
                    // CC `:641-648` returns
                    // `{behavior:'allow', updatedInput: acceptEditsResult.updatedInput ?? input,
                    //   decisionReason:{type:'mode', mode:'auto'}}`. The
                    // `?? input` fallback is identity here (an absent
                    // updatedInput means "execute the request input", which is
                    // what None already does downstream). The `mode` reason
                    // rides along even though its only CC consumer maps it to
                    // the same `'config'` an absent reason yields
                    // (`toolExecution.ts:236-244`).
                    return AutoModePreClassifier::Decided(HasPermissionsToUseToolResult::Allow {
                        updated_input: Some(updated_input.unwrap_or_else(|| params.input.clone())),
                        user_modified: None,
                        decision_reason: Some(PermissionDecisionReason::Mode {
                            mode: PermissionMode::Auto,
                        }),
                        // CC's literal (`:641-648`) sets no further allow fields.
                        tool_use_id: None,
                        accept_feedback: None,
                        content_blocks: Vec::new(),
                    });
                }
            }
        }
    }

    // Maps to: CC `:658-686` — the safe-tool allowlist skips the classifier
    // API call entirely.
    if crate::utils::permissions::classifier_decision::is_auto_mode_allowlisted_tool(
        params.tool_name,
    ) {
        // CC `:661-662` recordSuccess + persistDenialState.
        let new_denial_state =
            crate::utils::permissions::denial_tracking::record_success(denial_state);
        persist_denial_tracking(
            params.app_store,
            params.local_denial_tracking.as_ref(),
            new_denial_state,
        );
        // CC `:663-665`.
        tracing::debug!(
            tool = params.tool_name,
            "Skipping auto mode classifier for {}: tool is on the safe allowlist",
            params.tool_name
        );
        // SEAM (analytics unported): CC `:666-677`
        // logEvent('tengu_auto_mode_decision', { fastPath: 'allowlist' }).
        //
        // CC `:678-685` returns `{behavior:'allow', updatedInput: input,
        // decisionReason:{type:'mode', mode:'auto'}}`.
        return AutoModePreClassifier::Decided(HasPermissionsToUseToolResult::Allow {
            updated_input: Some(params.input.clone()),
            user_modified: None,
            decision_reason: Some(PermissionDecisionReason::Mode {
                mode: PermissionMode::Auto,
            }),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        });
    }

    AutoModePreClassifier::Classify(request)
}

/// Apply classifier to a would-be ask (sync bridge).
/// Prefer [`apply_auto_mode_classifier_to_ask_async`] from async contexts.
fn apply_auto_mode_classifier_to_ask(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    use crate::utils::permissions::yolo_classifier::classify_yolo_action_sync_with_signal;

    let mut params = params;
    let request = match auto_mode_pre_classifier_fast_path(&mut params, request, tool_use_context) {
        AutoModePreClassifier::Decided(result) => return result,
        AutoModePreClassifier::Classify(request) => request,
    };

    // Maps to CC `yoloClassifier.ts:1023-1029` — `actionCompact === ''` means
    // the tool declares no classifier-relevant input, and the classifier returns
    // without a request. CC evaluates that INSIDE the classifier, after
    // compaction; this is the same question asked one frame earlier, which is
    // why only its emptiness is consulted here and never its value.
    if projected_classifier_input(params.tool_name, params.input).is_none() {
        return resolve_auto_mode_classifier_decision(
            params,
            request,
            crate::utils::permissions::yolo_classifier::YoloClassifierDecision::Allow {
                reason: "Tool declares no classifier-relevant input".to_string(),
            },
        );
    }
    // Maps to: CC `classifyYoloAction(context.messages, action, ..., signal)`.
    // Sync bridge kept for orchestration / thread workers until they await.
    let decision = classify_yolo_action_sync_with_signal(
        params.messages,
        params.tool_name,
        // RAW input, as in the async path: the per-tool projection belongs to
        // `to_compact` and must run exactly once.
        params.input,
        classifier_tool_lookup(tool_use_context),
        params.context,
        params.abort_signal.clone(),
    );
    resolve_auto_mode_classifier_decision(params, request, decision)
}

/// Async classifier path — Maps to CC `await classifyYoloAction(...)`.
async fn apply_auto_mode_classifier_to_ask_async(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
) -> HasPermissionsToUseToolResult {
    use crate::utils::permissions::yolo_classifier::{
        classify_yolo_action, yolo_result_to_decision,
    };

    let mut params = params;
    let request = match auto_mode_pre_classifier_fast_path(&mut params, request, tool_use_context) {
        AutoModePreClassifier::Decided(result) => return result,
        AutoModePreClassifier::Classify(request) => request,
    };

    // Force-env / test skip stay inside classify_yolo_action_with_signal via
    // the sync force helpers; for pure async we still honor force env first.
    #[cfg(test)]
    if let Some(forced) =
        crate::utils::permissions::yolo_classifier::forced_classifier_decision(params.tool_name)
    {
        return resolve_auto_mode_classifier_decision(params, request, forced);
    }

    // Maps to CC `yoloClassifier.ts:1023-1029` — `actionCompact === ''` means
    // the tool declares no classifier-relevant input, and the classifier returns
    // without a request. CC evaluates that INSIDE the classifier, after
    // compaction; this is the same question asked one frame earlier, which is
    // why only its emptiness is consulted here and never its value.
    if projected_classifier_input(params.tool_name, params.input).is_none() {
        return resolve_auto_mode_classifier_decision(
            params,
            request,
            crate::utils::permissions::yolo_classifier::YoloClassifierDecision::Allow {
                reason: "Tool declares no classifier-relevant input".to_string(),
            },
        );
    }
    // CC `permissions.ts` reaches `classifyYoloAction` with the action already
    // shaped as a `TranscriptEntry` (`formatActionForClassifier`); the pair form
    // was a Rust-side narrowing that shut the handoff caller out entirely.
    //
    // The entry carries the RAW input. CC projects exactly once, inside
    // `toCompact` → `tool.toAutoClassifierInput` (`yoloClassifier.ts:400`).
    // Handing the projected value in here would encode it twice, the second
    // pass running `to_auto_classifier_input` over an already-projected string.
    let action = crate::utils::permissions::yolo_classifier::format_action_for_classifier(
        params.tool_name,
        params.input.clone(),
    );
    let result = classify_yolo_action(
        params.messages,
        &action,
        classifier_tool_lookup(tool_use_context),
        params.context,
        params.abort_signal.clone(),
    )
    .await;
    let decision = yolo_result_to_decision(result);
    resolve_auto_mode_classifier_decision(params, request, decision)
}

/// Shared post-classify branch (unavailable / allow / block + denial tracking).
/// Maps to: CC `utils/permissions/permissions.ts:818-921`:
/// - unavailable → `tengu_iron_gate_closed` decides deny vs the untouched ask
/// - shouldBlock → deny (+ denial limit → ask with warning)
/// - allow → allow + recordSuccess
fn resolve_auto_mode_classifier_decision(
    params: HasPermissionsToUseToolParams<'_>,
    request: PermissionRequest,
    decision: crate::utils::permissions::yolo_classifier::YoloClassifierDecision,
) -> HasPermissionsToUseToolResult {
    use crate::utils::permissions::denial_tracking::{
        create_denial_tracking_state, record_denial, record_success, should_fallback_to_prompting,
    };
    use crate::utils::permissions::yolo_classifier::YoloClassifierDecision;

    // Maps to: CC `context.localDenialTracking ?? appState.denialTracking`.
    let mut denial_state =
        load_denial_tracking(params.app_store, params.local_denial_tracking.as_ref());

    match decision {
        YoloClassifierDecision::Unavailable {
            transcript_too_long: true,
            ..
        }
        | YoloClassifierDecision::Block {
            transcript_too_long: true,
            ..
        } => {
            // CC permissions.ts:822-841: deterministic overflow bypasses iron gate.
            if params.context.should_avoid_permission_prompts {
                return HasPermissionsToUseToolResult::Aborted(
                    crate::utils::errors::AbortError::new(
                        "Agent aborted: auto mode classifier transcript exceeded context window in headless mode",
                    ),
                );
            }
            let mut request = request;
            request.decision_reason = Some(PermissionDecisionReason::Other {
                reason: "Auto mode classifier transcript exceeded context window — falling back to manual approval".into(),
            });
            HasPermissionsToUseToolResult::Ask(request)
        }
        YoloClassifierDecision::Unavailable { reason, model, .. } => {
            resolve_classifier_unavailable(
                params.tool_name,
                request,
                &reason,
                &model,
                crate::utils::feature_flags::feature_enabled(
                    crate::utils::feature_flags::FeatureFlag::IronGateClosed,
                ),
            )
        }
        YoloClassifierDecision::Allow { reason } => {
            denial_state = record_success(denial_state);
            persist_denial_tracking(
                params.app_store,
                params.local_denial_tracking.as_ref(),
                denial_state,
            );
            crate::utils::classifier_approvals::set_yolo_classifier_approval(
                params.tool_use_id,
                &reason,
            );
            HasPermissionsToUseToolResult::Allow {
                updated_input: Some(params.input.clone()),
                user_modified: None,
                decision_reason: Some(PermissionDecisionReason::Classifier {
                    classifier: "auto-mode".to_string(),
                    reason,
                }),
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            }
        }
        YoloClassifierDecision::Block { reason, .. } => {
            denial_state = record_denial(denial_state);
            // CC permissions.ts:880-900: persist before handling either limit.
            persist_denial_tracking(
                params.app_store,
                params.local_denial_tracking.as_ref(),
                denial_state,
            );
            // Maps to: CC handleDenialLimitExceeded — fall back to prompting with
            // classifier reason.
            if should_fallback_to_prompting(denial_state) {
                // CC :1023-1027: headless aborts without clearing the counters.
                if params.context.should_avoid_permission_prompts {
                    return HasPermissionsToUseToolResult::Aborted(
                        crate::utils::errors::AbortError::new(
                            "Agent aborted: too many classifier denials in headless mode",
                        ),
                    );
                }
                let hit_total = denial_state.total_denials
                    >= crate::utils::permissions::denial_tracking::DENIAL_LIMITS.max_total;
                let warning = if hit_total {
                    format!(
                        "{} actions were blocked this session. Please review the transcript before continuing.",
                        denial_state.total_denials
                    )
                } else {
                    format!(
                        "{} consecutive actions were blocked. Please review the transcript before continuing.",
                        denial_state.consecutive_denials
                    )
                };
                if hit_total {
                    denial_state = create_denial_tracking_state();
                    persist_denial_tracking(
                        params.app_store,
                        params.local_denial_tracking.as_ref(),
                        denial_state,
                    );
                }
                // Maps to: CC `handleDenialLimitExceeded` `:1045-1057`. The
                // warning rides on `decisionReason`, NOT on the decision's
                // `message`: CC returns `{...result, decisionReason: {...}}`, so
                // `message` keeps `createPermissionRequestMessage(tool.name)`
                // and the dialog's own `description` stays
                // `await tool.description(input, …)`
                // (`useCanUseTool.tsx:138-143`). This is what
                // `PermissionRuleExplanation`'s auto-mode classifier arm renders
                // in `error` colour (`PermissionRuleExplanation.tsx:37-42`);
                // writing it into `description` instead put it on the dim line
                // and left that arm with no producer.
                //
                // CC `:1045-1048` `originalClassifier`: keep an already-present
                // classifier name (e.g. 'dangerous-agent-action') and default to
                // 'auto-mode' otherwise.
                let mut ask_req = request;
                let classifier = match &ask_req.decision_reason {
                    Some(PermissionDecisionReason::Classifier { classifier, .. }) => {
                        classifier.clone()
                    }
                    _ => "auto-mode".to_string(),
                };
                ask_req.decision_reason = Some(PermissionDecisionReason::Classifier {
                    classifier,
                    reason: format!("{warning}\n\nLatest blocked action: {reason}"),
                });
                return HasPermissionsToUseToolResult::Ask(ask_req);
            }

            let mut deny_req = request;
            // Maps to: CC `:903-911` — the block deny carries
            // `decisionReason: {type:'classifier', classifier:'auto-mode', reason}`.
            // That reason is the real predicate behind
            // `toolExecution.ts:1075-1078`'s PermissionDenied hook run; the
            // `[ClassifierBlocked] ` prefix this field used to carry was an
            // invented string that existed only because the reason was dropped.
            //
            // `message` is `buildYoloRejectionMessage(reason)` (`:910`), the copy
            // the MODEL sees via `toolExecution.ts:1023`. It used to land on
            // `description` — the tool's own self-description — because
            // `PermissionRequest` had no `message` field.
            deny_req.decision_reason = Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: reason.clone(),
            });
            deny_req.message = crate::utils::messages::build_yolo_rejection_message(&reason);
            HasPermissionsToUseToolResult::Deny(deny_req)
        }
    }
}

/// Maps to: CC denial state load from localDenialTracking ?? appState.denialTracking.
fn load_denial_tracking(
    app_store: Option<&crate::state::store::AppStore>,
    local: Option<&crate::tool::SharedDenialTracking>,
) -> crate::utils::permissions::denial_tracking::DenialTrackingState {
    use crate::utils::permissions::denial_tracking::create_denial_tracking_state;
    // CC `context.localDenialTracking ?? appState.denialTracking ??
    // createDenialTrackingState()` (`permissions.ts:490`, `:554-557`). Nullish,
    // not truthy: a present local wins even at zero counters.
    if let Some(local) = local {
        return local.get();
    }
    if let Some(store) = app_store {
        if let Some(state) = store.get().denial_tracking {
            return state;
        }
    }
    create_denial_tracking_state()
}

/// Maps to: CC `persistDenialState` — local Object.assign or setAppState.
fn persist_denial_tracking(
    app_store: Option<&crate::state::store::AppStore>,
    local: Option<&crate::tool::SharedDenialTracking>,
    new_state: crate::utils::permissions::denial_tracking::DenialTrackingState,
) {
    // CC `permissions.ts:967-968`: `Object.assign(context.localDenialTracking,
    // newState)` — an in-place write to the shared object, and the `else` below
    // is the only path that reaches appState. An isolated subagent always has
    // this set (`forkedAgent.ts:422`), which is exactly what keeps its denials
    // out of the parent's store.
    if let Some(local) = local {
        local.set(new_state);
        return;
    }
    if let Some(store) = app_store {
        // B3 flip-audit: CC permissions.ts:974 — `if (prev.denialTracking ===
        // newState) return prev` ("recordSuccess returns the same reference
        // when state is unchanged"); DenialTrackingState is Copy+PartialEq,
        // value equality is the exact projection of that same-reference
        // early-return (denialTracking.ts:33 returns `state` untouched).
        store.set_state(|prev| {
            if prev.denial_tracking == Some(new_state) {
                return crate::state::store::UpdateDecision::Same(());
            }
            let mut next = (**prev).clone();
            next.denial_tracking = Some(new_state);
            crate::state::store::UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
    }
}

/// Maps to: CC `utils/permissions/permissions.ts:845-875` — the classifier
/// `unavailable` branch, gated on `tengu_iron_gate_closed` (default true).
///
/// `iron_gate_closed` is the resolved gate value so the fail-open half stays
/// reachable from tests while the source-controlled switch stays true.
fn resolve_classifier_unavailable(
    tool_name: &str,
    request: PermissionRequest,
    reason: &str,
    model: &str,
    iron_gate_closed: bool,
) -> HasPermissionsToUseToolResult {
    if iron_gate_closed {
        tracing::warn!(
            tool = tool_name,
            reason = %reason,
            "Auto mode classifier unavailable, denying with retry guidance (fail closed)"
        );
        let mut deny_req = request;
        // Maps to: CC `:857-868` — the fail-closed deny carries
        // `decisionReason: {type:'classifier', classifier:'auto-mode',
        // reason:'Classifier unavailable'}` alongside its message.
        deny_req.decision_reason = Some(PermissionDecisionReason::Classifier {
            classifier: "auto-mode".to_string(),
            reason: "Classifier unavailable".to_string(),
        });
        deny_req.message =
            crate::utils::messages::build_classifier_unavailable_message(tool_name, model);
        return HasPermissionsToUseToolResult::Deny(deny_req);
    }
    tracing::warn!(
        tool = tool_name,
        reason = %reason,
        "Auto mode classifier unavailable, falling back to normal permission handling (fail open)"
    );
    HasPermissionsToUseToolResult::Ask(request)
}

/// Maps to: CC `utils/permissions/permissions.ts:1244-1250` (step 1f, a
/// content-specific ask rule from `tool.checkPermissions`) and `:1255-1260`
/// (step 1g, a `safetyCheck` ask). Both `return toolPermissionResult` from
/// `hasPermissionsToUseToolInner`, i.e. above step 2a's bypassPermissions allow
/// at `:1272-1281` and step 2b's whole-tool allow at `:1288-1297`. That is the
/// only thing these two steps decide.
///
/// Step 1g deliberately does NOT read `classifierApprovable`: bypass immunity is
/// unconditional for a safetyCheck. Whether the AUTO-mode classifier may still
/// evaluate it is a separate question answered one layer up, in the outer entry's
/// `:532-548` (see `maybe_classifier_for_ask`).
fn tool_permission_ask_is_bypass_immune(
    reason: Option<&crate::utils::permissions::permission_result::PermissionDecisionReason>,
) -> bool {
    use crate::utils::permissions::permission_result::PermissionDecisionReason;
    match reason {
        Some(PermissionDecisionReason::SafetyCheck { .. }) => true,
        Some(PermissionDecisionReason::Rule { rule }) => {
            rule.rule_behavior == PermissionBehavior::Ask
        }
        _ => false,
    }
}

/// `message` maps to the REQUIRED `message` on CC's ask/deny/passthrough
/// decisions (`types/permissions.ts:203`, `:233`, `:257`). It is the decision's
/// own explanation, not the tool's `description(input)` — see
/// `PermissionRequest::message`.
fn permission_request_from_tool_result(
    params: &HasPermissionsToUseToolParams<'_>,
    suggestions: Option<&[PermissionUpdate]>,
    is_compound_command: bool,
    decision_reason: Option<PermissionDecisionReason>,
    message: String,
) -> PermissionRequest {
    let suggested_rule = suggestions
        .unwrap_or_default()
        .iter()
        .find_map(|update| match update {
            PermissionUpdate::AddRules { rules, .. }
            | PermissionUpdate::ReplaceRules { rules, .. } => rules.first().cloned(),
            _ => None,
        });
    let rule = suggested_rule.unwrap_or_else(|| {
        PermissionRuleValue::new(
            permission_rule_tool_name(params.tool_name),
            Some(params.input_summary.to_string()),
        )
    });
    let mut request = mock_permission_request_with_input(
        format!("perm-{}", params.tool_use_id),
        params.tool_use_id.to_string(),
        params.tool_name.to_string(),
        params.input_summary.to_string(),
        params.input.clone(),
        params.context.mode,
    );
    request.rule = rule;
    request.mcp_info = params.mcp_info.cloned();
    request.suggestions = suggestions.unwrap_or_default().to_vec();
    request.is_compound_command = is_compound_command;
    request.decision_reason = decision_reason;
    request.message = message;
    request
}

fn permission_rule_tool_name(tool_name: &str) -> &str {
    match tool_name {
        "Write" | "FileWrite" | "Edit" | "FileEdit" | "NotebookEdit" => "Edit",
        _ => tool_name,
    }
}

fn permission_rule_content_for_tool(
    tool_name: &str,
    input: &serde_json::Value,
    fallback: &str,
) -> String {
    if tool_name == "WebFetch" {
        return web_fetch_permission_rule_content(input, fallback);
    }
    fallback.to_string()
}

/// Maps to CC `WebFetchTool.ts` `webFetchToolInputToPermissionRuleContent(...)`.
fn web_fetch_permission_rule_content(input: &serde_json::Value, fallback: &str) -> String {
    input
        .get("url")
        .and_then(|value| value.as_str())
        .and_then(crate::tools::web_fetch_tool::preapproved::parse_web_fetch_url_host_path)
        .map(|(hostname, _)| format!("domain:{hostname}"))
        .unwrap_or_else(|| format!("input:{fallback}"))
}

/// Maps to: CC accept-edits mode file permission checks inside
/// `utils/permissions/permissions.ts`.

fn normalized_absolute_permission_path(path: &str) -> Option<std::path::PathBuf> {
    if path.starts_with("//") || path.starts_with("\\\\") {
        return None;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return None;
    };
    let path = std::path::PathBuf::from(path);
    let absolute_path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    Some(normalize_permission_path(&absolute_path))
}

fn path_is_inside_working_dir_normalized(path: &std::path::Path) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    path.starts_with(normalize_permission_path(&cwd))
}

fn normalize_permission_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_has_sensitive_component(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy();
        matches!(value.as_ref(), ".git" | ".claude" | ".vscode" | ".idea")
    })
}

/// Maps to: CC `utils/permissions/permissions.ts:238-269#toolMatchesRule`.
///
/// CC hands this the `Tool` and resolves `nameForRuleMatch` through
/// `getToolNameForPermissionCheck(tool)`. The Rust seam only receives the
/// requested `PermissionRuleValue`, so `mcp_info` rides alongside it and takes
/// priority over the requested name — otherwise an unprefixed SDK MCP tool
/// (`CLAUDE_AGENT_SDK_MCP_NO_PREFIX`) would share a rule-match name with the
/// builtin it shadows.
fn tool_matches_rule(
    requested: &PermissionRuleValue,
    mcp_info: Option<&McpToolInfo>,
    rule: &PermissionRule,
) -> bool {
    if rule.rule_value.rule_content.is_some() {
        return false;
    }

    let resolved_name = get_tool_name_for_permission_check(requested.tool_name.as_str(), mcp_info);
    let name_for_rule_match = resolved_name.as_ref();

    if rule.rule_value.tool_name == name_for_rule_match {
        return true;
    }

    let Some(rule_info) = mcp_info_from_string(&rule.rule_value.tool_name) else {
        return false;
    };
    let Some(tool_info) = mcp_info_from_string(name_for_rule_match) else {
        return false;
    };

    matches!(rule_info.tool_name.as_deref(), None | Some("*"))
        && rule_info.server_name == tool_info.server_name
}

/// Maps to CC `utils/permissions/permissions.ts:275-283#toolAlwaysAllowedRule`.
fn tool_always_allowed_rule(
    context: &ToolPermissionContext,
    requested: &PermissionRuleValue,
    mcp_info: Option<&McpToolInfo>,
) -> Option<PermissionRule> {
    get_allow_rules(context)
        .into_iter()
        .find(|rule| tool_matches_rule(requested, mcp_info, rule))
}

/// Maps to: CC `utils/permissions/permissions.ts:287-293#getDenyRuleForTool`.
pub fn get_deny_rule_for_tool(
    context: &ToolPermissionContext,
    requested: &PermissionRuleValue,
    mcp_info: Option<&McpToolInfo>,
) -> Option<PermissionRule> {
    get_deny_rules(context)
        .into_iter()
        .find(|rule| tool_matches_rule(requested, mcp_info, rule))
}

/// Maps to: CC `utils/permissions/permissions.ts:297-303#getAskRuleForTool`.
fn get_ask_rule_for_tool(
    context: &ToolPermissionContext,
    requested: &PermissionRuleValue,
    mcp_info: Option<&McpToolInfo>,
) -> Option<PermissionRule> {
    get_ask_rules(context)
        .into_iter()
        .find(|rule| tool_matches_rule(requested, mcp_info, rule))
}

fn has_matching_rule(
    rules_by_source: &ToolPermissionRulesBySource,
    requested: &PermissionRuleValue,
) -> bool {
    PERMISSION_RULE_SOURCES.iter().any(|source| {
        rules_by_source.get(source).is_some_and(|rules| {
            rules.iter().any(|rule| {
                rule.tool_name == requested.tool_name
                    && match (&rule.rule_content, &requested.rule_content) {
                        (None, _) => true,
                        (Some(expected), Some(actual)) => {
                            permission_rule_content_matches(&requested.tool_name, expected, actual)
                        }
                        (Some(expected), None) => expected.is_empty(),
                    }
            })
        })
    })
}

fn plan_mode_updates_for_choice(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
) -> Option<Vec<PermissionUpdate>> {
    match (request.tool_name.as_str(), choice) {
        // Maps to: CC `EnterPlanModePermissionRequest.handleResponse('yes')`
        // `{ type: 'setMode', mode: 'plan', destination: 'session' }`.
        ("EnterPlanMode", PermissionPromptChoice::AllowOnce)
        | ("EnterPlanMode", PermissionPromptChoice::AlwaysAllow) => {
            Some(vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::Plan,
            }])
        }
        // Maps to the ported ExitPlanMode dialog subset:
        // - `Yes, auto-accept edits` -> acceptEdits
        // - `Yes, manually approve edits` -> default
        ("ExitPlanMode", PermissionPromptChoice::AlwaysAllow) => {
            Some(vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            }])
        }
        ("ExitPlanMode", PermissionPromptChoice::AllowOnce) => {
            Some(vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::Default,
            }])
        }
        _ => None,
    }
}

pub fn decision_for_choice(
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
) -> PromptDecision {
    match choice {
        PermissionPromptChoice::AllowOnce => PromptDecision {
            behavior: PermissionBehavior::Allow,
            choice,
            updates: plan_mode_updates_for_choice(request, choice).unwrap_or_default(),
            transcript: String::new(),
            updated_input: None,
        },
        PermissionPromptChoice::AlwaysAllow => PromptDecision {
            behavior: PermissionBehavior::Allow,
            choice,
            updates: plan_mode_updates_for_choice(request, choice).unwrap_or_else(|| {
                vec![PermissionUpdate::AddRules {
                    destination: PermissionUpdateDestination::Session,
                    behavior: PermissionBehavior::Allow,
                    rules: vec![request.rule.clone()],
                }]
            }),
            transcript: String::new(),
            updated_input: None,
        },
        PermissionPromptChoice::Deny => PromptDecision {
            behavior: PermissionBehavior::Deny,
            choice,
            updates: Vec::new(),
            transcript: String::new(),
            updated_input: None,
        },
    }
}

/// Maps to CC `ToolUseConfirm.onAllow(updatedInput, permissionUpdates, ...)`
/// resolving into a permission decision and mutating the live permission
/// context before tool execution resumes.
pub fn apply_prompt_response(
    context: &ToolPermissionContext,
    request: &PermissionRequest,
    response: &PermissionPromptResponse,
) -> (ToolPermissionContext, PromptDecision) {
    let mut decision = decision_for_choice(request, response.choice);
    if matches!(decision.behavior, PermissionBehavior::Allow) {
        if response.permission_updates_explicit || !response.permission_updates.is_empty() {
            decision.updates = response.permission_updates.clone();
        }
        decision.updated_input = response.updated_input.clone();
    }
    let next_context = apply_permission_updates(context, &decision.updates);
    (next_context, decision)
}

pub fn apply_prompt_choice(
    context: &ToolPermissionContext,
    request: &PermissionRequest,
    choice: PermissionPromptChoice,
) -> (ToolPermissionContext, PromptDecision) {
    apply_prompt_response(context, request, &PermissionPromptResponse::new(choice))
}

/// Maps to: CC `utils/permissions/permissions.ts:1329-1374#deletePermissionRule`.
pub fn delete_permission_rule(
    context: &ToolPermissionContext,
    rule: &PermissionRule,
) -> anyhow::Result<ToolPermissionContext> {
    let destination = match rule.source {
        PermissionRuleSource::PolicySettings
        | PermissionRuleSource::FlagSettings
        | PermissionRuleSource::Command => {
            anyhow::bail!("Cannot delete permission rules from read-only settings")
        }
        PermissionRuleSource::UserSettings => PermissionUpdateDestination::UserSettings,
        PermissionRuleSource::ProjectSettings => PermissionUpdateDestination::ProjectSettings,
        PermissionRuleSource::LocalSettings => PermissionUpdateDestination::LocalSettings,
        PermissionRuleSource::CliArg => PermissionUpdateDestination::CliArg,
        PermissionRuleSource::Session => PermissionUpdateDestination::Session,
    };
    let update = PermissionUpdate::RemoveRules {
        destination,
        behavior: rule.rule_behavior,
        rules: vec![rule.rule_value.clone()],
    };
    let updated = super::permission_update::apply_permission_update(context, &update);
    if matches!(
        rule.source,
        PermissionRuleSource::UserSettings
            | PermissionRuleSource::ProjectSettings
            | PermissionRuleSource::LocalSettings
    ) {
        // CC :1352-1363 ignores the loader's boolean result. A failed disk
        // deletion still projects the removed rule into the live context.
        super::permissions_loader::delete_permission_rule_from_settings(rule);
    }
    Ok(updated)
}

/// Maps to: CC `utils/permissions/permissions.ts:1403-1412#applyPermissionRulesToPermissionContext`.
pub fn apply_permission_rules_to_permission_context(
    context: &ToolPermissionContext,
    rules: &[PermissionRule],
) -> ToolPermissionContext {
    let mut next = context.clone();
    for rule in rules {
        let rules_by_source = match rule.rule_behavior {
            PermissionBehavior::Allow => &mut next.always_allow_rules,
            PermissionBehavior::Deny => &mut next.always_deny_rules,
            PermissionBehavior::Ask => &mut next.always_ask_rules,
        };
        rules_by_source
            .entry(rule.source)
            .or_default()
            .push(rule.rule_value.clone());
    }
    next
}

/// Maps to: CC `utils/permissions/permissions.ts:1419-1471#syncPermissionRulesFromDisk`.
/// Replaces the literal source/behavior groups handled by upstream reload.
/// Managed-only additionally clears CLI and session, while command and flag
/// survive; policy/flag groups are replaced only when the fresh loader emits
/// that exact group.
pub fn sync_permission_rules_from_disk(
    context: &ToolPermissionContext,
    rules: &[PermissionRule],
    managed_only: bool,
) -> ToolPermissionContext {
    let mut next = context.clone();
    let mut sources_to_clear = vec![
        PermissionRuleSource::UserSettings,
        PermissionRuleSource::ProjectSettings,
        PermissionRuleSource::LocalSettings,
    ];
    if managed_only {
        sources_to_clear.extend([PermissionRuleSource::CliArg, PermissionRuleSource::Session]);
    }
    for source in sources_to_clear {
        next.always_allow_rules.insert(source, Vec::new());
        next.always_deny_rules.insert(source, Vec::new());
        next.always_ask_rules.insert(source, Vec::new());
    }

    let mut grouped =
        IndexMap::<(PermissionRuleSource, PermissionBehavior), Vec<PermissionRuleValue>>::new();
    for rule in rules {
        grouped
            .entry((rule.source, rule.rule_behavior))
            .or_default()
            .push(rule.rule_value.clone());
    }
    for ((source, behavior), rule_values) in grouped {
        let rules_by_source = match behavior {
            PermissionBehavior::Allow => &mut next.always_allow_rules,
            PermissionBehavior::Deny => &mut next.always_deny_rules,
            PermissionBehavior::Ask => &mut next.always_ask_rules,
        };
        rules_by_source.insert(source, rule_values);
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn permission_decision_allow_once_has_no_rule_update() {
        let request = mock_permission_request(
            "p1",
            "toolu_1",
            "Bash",
            "cargo test",
            PermissionMode::Default,
        );
        assert!(request.description.is_empty());
        // Maps to: CC `PermissionRequest.tsx:139` — no `decisionReason` until a
        // rule matches or a tool supplies one.
        assert!(request.decision_reason.is_none());
        let (ctx, decision) = apply_prompt_choice(
            &ToolPermissionContext::default(),
            &request,
            PermissionPromptChoice::AllowOnce,
        );

        assert_eq!(decision.behavior, PermissionBehavior::Allow);
        assert!(decision.updates.is_empty());
        assert!(!has_in_memory_allow_rule(&ctx, &request.rule));
    }

    /// Maps to: CC `permissions.ts:1184-1203` (step 1b) vs `:1299-1310` (step
    /// 3). A tool-wide ask rule attaches `decisionReason: {type:'rule', rule}`
    /// carrying the MATCHED rule; a decision that only became `ask` through the
    /// passthrough conversion carries no reason at all, which is what makes
    /// `PermissionRuleExplanation` render nothing.
    #[test]
    fn ask_rule_match_matches_official_rule_decision_reason() {
        use crate::utils::permissions::permission_result::PermissionDecisionReason;

        let mut context = ToolPermissionContext::default();
        context.always_ask_rules.insert(
            PermissionRuleSource::LocalSettings,
            vec![PermissionRuleValue::new("TeamCreate", None)],
        );
        let input = serde_json::json!({"team_name": "reviewers"});
        let request = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_team_create",
            tool_name: "TeamCreate",
            mcp_info: None,
            input_summary: "reviewers",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected ask, got {other:?}"),
        };
        match request.decision_reason {
            Some(PermissionDecisionReason::Rule { ref rule }) => {
                assert_eq!(rule.rule_value.tool_name, "TeamCreate");
                assert_eq!(rule.rule_value.rule_content, None);
                assert_eq!(rule.rule_behavior, PermissionBehavior::Ask);
                assert_eq!(rule.source, PermissionRuleSource::LocalSettings);
            }
            other => panic!("expected rule decision reason, got {other:?}"),
        }
        // The requested rule stays the input projection; only the reason
        // carries the matched rule.
        assert_eq!(
            request.rule.rule_content.as_deref(),
            Some("reviewers"),
            "request.rule must remain the requested (input) rule"
        );

        // An unregistered name is CC's passthrough seed (`:1208-1213`), so the
        // step-3 conversion produces an `ask` with no `decisionReason`.
        let plain = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_unregistered_plain",
            tool_name: "CustomTool",
            mcp_info: None,
            input_summary: "reviewers",
            input: &input,
            context: &ToolPermissionContext::default(),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected ask, got {other:?}"),
        };
        assert!(plain.decision_reason.is_none());
    }

    /// Maps to: CC `permissions.ts:1170-1181` (step 1a).
    #[test]
    fn deny_rule_match_matches_official_rule_decision_reason() {
        use crate::utils::permissions::permission_result::PermissionDecisionReason;

        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new("TeamCreate", None)],
        );
        let input = serde_json::json!({"team_name": "reviewers"});
        let request = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_team_create_deny",
            tool_name: "TeamCreate",
            mcp_info: None,
            input_summary: "reviewers",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Deny(request) => request,
            other => panic!("expected deny, got {other:?}"),
        };
        assert!(matches!(
            request.decision_reason,
            Some(PermissionDecisionReason::Rule { ref rule })
                if rule.rule_behavior == PermissionBehavior::Deny
                    && rule.source == PermissionRuleSource::UserSettings
        ));
    }

    #[test]
    fn prompt_response_permission_updates_override_default_always_allow_rule() {
        let request = mock_permission_request(
            "p1",
            "toolu_1",
            "Bash",
            "cargo test",
            PermissionMode::Default,
        );
        let response = PermissionPromptResponse::new(PermissionPromptChoice::AlwaysAllow)
            .with_permission_updates(vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "Bash",
                    Some("cargo test --quiet".to_string()),
                )],
            }]);

        let (ctx, decision) =
            apply_prompt_response(&ToolPermissionContext::default(), &request, &response);

        assert_eq!(decision.updates, response.permission_updates);
        assert!(
            ctx.always_allow_rules
                .get(&PermissionRuleSource::LocalSettings)
                .is_some_and(|rules| rules.contains(&PermissionRuleValue::new(
                    "Bash",
                    Some("cargo test --quiet".to_string())
                )))
        );
        assert!(!has_in_memory_allow_rule(&ctx, &request.rule));
    }

    #[test]
    fn explicit_empty_prompt_response_updates_suppress_legacy_always_allow_rule() {
        let request = mock_permission_request(
            "p-file",
            "toolu_file",
            "Edit",
            "src/main.rs",
            PermissionMode::AcceptEdits,
        );
        let response = PermissionPromptResponse::new(PermissionPromptChoice::AlwaysAllow)
            .with_permission_updates(Vec::new());

        let (ctx, decision) =
            apply_prompt_response(&ToolPermissionContext::default(), &request, &response);

        assert!(decision.updates.is_empty());
        assert!(!has_in_memory_allow_rule(&ctx, &request.rule));
    }

    #[test]
    fn plan_mode_permission_choices_apply_official_mode_updates() {
        let enter = mock_permission_request(
            "p-enter",
            "toolu_enter",
            "EnterPlanMode",
            "",
            PermissionMode::Default,
        );
        let (ctx, decision) = apply_prompt_choice(
            &ToolPermissionContext::default(),
            &enter,
            PermissionPromptChoice::AllowOnce,
        );
        assert_eq!(ctx.mode, PermissionMode::Plan);
        assert_eq!(
            decision.updates,
            vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::Plan,
            }]
        );

        let exit = mock_permission_request(
            "p-exit",
            "toolu_exit",
            "ExitPlanMode",
            "",
            PermissionMode::Plan,
        );
        let (ctx, decision) = apply_prompt_choice(&ctx, &exit, PermissionPromptChoice::AlwaysAllow);
        assert_eq!(ctx.mode, PermissionMode::AcceptEdits);
        assert_eq!(
            decision.updates,
            vec![PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            }]
        );
    }

    #[test]
    fn permission_rules_match_whole_tool_or_exact_content() {
        let mut ctx = ToolPermissionContext::default();
        ctx.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        ctx.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Read",
                Some("README.md".to_string()),
            )],
        );

        assert!(has_in_memory_deny_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("echo anything".to_string()))
        ));
        assert!(has_in_memory_ask_rule(
            &ctx,
            &PermissionRuleValue::new("Read", Some("README.md".to_string()))
        ));
        assert!(!has_in_memory_ask_rule(
            &ctx,
            &PermissionRuleValue::new("Read", Some("src/main.rs".to_string()))
        ));
    }

    #[test]
    fn read_outer_rules_and_filesystem_order_match_official() {
        let input = serde_json::json!({"file_path": "/tmp/.../secret.txt"});

        let mut content_denied = ToolPermissionContext::default();
        content_denied.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", Some("**".to_string()))],
        );
        let tool_context =
            crate::tool::ToolUseContext::with_permission_context(content_denied.clone());
        let result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_read_suspicious_content_deny",
                tool_name: "Read",
                mcp_info: None,
                input_summary: "/tmp/.../secret.txt",
                input: &input,
                context: &content_denied,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&tool_context),
        );
        assert!(matches!(result, HasPermissionsToUseToolResult::Ask(_)));

        let mut whole_tool_denied = content_denied;
        whole_tool_denied.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", None)],
        );
        let tool_context =
            crate::tool::ToolUseContext::with_permission_context(whole_tool_denied.clone());
        let result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_read_suspicious_tool_deny",
                tool_name: "Read",
                mcp_info: None,
                input_summary: "/tmp/.../secret.txt",
                input: &input,
                context: &whole_tool_denied,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&tool_context),
        );
        assert!(matches!(result, HasPermissionsToUseToolResult::Deny(_)));
    }

    #[test]
    fn read_permission_parse_retains_raw_input_matches_official() {
        let raw_valid = serde_json::json!({"file_path": "/tmp/.../secret.txt", "offset": "2"});
        let valid_context = ToolPermissionContext::default();
        let valid_tool_context =
            crate::tool::ToolUseContext::with_permission_context(valid_context.clone());
        let valid_result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_read_valid_permission_input",
                tool_name: "Read",
                mcp_info: None,
                input_summary: "/tmp/.../secret.txt",
                input: &raw_valid,
                context: &valid_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&valid_tool_context),
        );
        let HasPermissionsToUseToolResult::Ask(valid_request) = valid_result else {
            panic!("suspicious Read path must ask")
        };
        assert_eq!(valid_request.input, raw_valid);
        assert_eq!(valid_request.input["offset"], serde_json::json!("2"));

        let raw = serde_json::json!({"file_path": 5, "offset": "2"});
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", Some("5".to_string()))],
        );
        let tool_context = crate::tool::ToolUseContext::with_permission_context(context.clone());
        let result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_read_invalid_permission_input",
                tool_name: "Read",
                mcp_info: None,
                input_summary: "5",
                input: &raw,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&tool_context),
        );
        let HasPermissionsToUseToolResult::Ask(request) = result else {
            panic!("schema-invalid Read must retain the passthrough/default ask")
        };
        assert_eq!(request.input, raw);

        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", None)],
        );
        let tool_context = crate::tool::ToolUseContext::with_permission_context(context.clone());
        assert_eq!(
            has_permissions_to_use_tool_with_context(
                HasPermissionsToUseToolParams {
                    tool_use_id: "toolu_read_invalid_whole_allow",
                    tool_name: "Read",
                    mcp_info: None,
                    input_summary: "5",
                    input: &raw,
                    context: &context,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: None,
                    abort_signal: None,
                },
                Some(&tool_context),
            ),
            expected_allow(
                &raw,
                PermissionDecisionReason::Rule {
                    rule: PermissionRule {
                        source: PermissionRuleSource::Session,
                        rule_behavior: PermissionBehavior::Allow,
                        rule_value: PermissionRuleValue::new("Read", None)
                    }
                }
            )
        );
    }

    #[test]
    fn permission_rules_match_legacy_prefix_and_wildcard_content() {
        let mut ctx = ToolPermissionContext::default();
        ctx.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![
                PermissionRuleValue::new("Bash", Some("cargo:*".to_string())),
                PermissionRuleValue::new("Read", Some("src/*.rs".to_string())),
                PermissionRuleValue::new("Bash", Some("git *".to_string())),
                PermissionRuleValue::new("Bash", Some("echo \\* *".to_string())),
            ],
        );

        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("cargo test".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("cargo".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Read", Some("src/main.rs".to_string()))
        ));
        assert!(!has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Read", Some("README.md".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("git".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("git status".to_string()))
        ));
        assert!(!has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("git status && rm -rf target".to_string()))
        ));
        assert!(has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("echo * literal".to_string()))
        ));
        assert!(!has_in_memory_allow_rule(
            &ctx,
            &PermissionRuleValue::new("Bash", Some("echo anything literal".to_string()))
        ));
    }

    #[test]
    fn read_working_directory_ask_is_bypassable_but_explicit_ask_is_not() {
        let root = std::env::temp_dir().join(format!(
            "cometix-read-bypass-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let outside = root.with_extension("outside").join("file.txt");
        let outside_text = outside.display().to_string();
        let input = serde_json::json!({"file_path": outside_text.clone()});
        let mut permissions = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        let context = crate::tool::ToolUseContext {
            cwd_override: Some(root),
            tool_permission_context: permissions.clone(),
            ..crate::tool::ToolUseContext::default()
        };
        let params = HasPermissionsToUseToolParams {
            tool_use_id: "toolu_read_bypass",
            tool_name: "Read",
            mcp_info: None,
            input_summary: &outside_text,
            input: &input,
            context: &permissions,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        };
        assert_eq!(
            has_permissions_to_use_tool_with_context(params, Some(&context)),
            expected_allow(
                &input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::BypassPermissions
                }
            )
        );

        permissions
            .always_ask_rules
            .entry(PermissionRuleSource::Session)
            .or_default()
            .push(PermissionRuleValue::new(
                "Read",
                Some(format!("/{outside_text}")),
            ));
        let explicit_context = crate::tool::ToolUseContext {
            tool_permission_context: permissions.clone(),
            ..context
        };
        assert!(matches!(
            has_permissions_to_use_tool_with_context(
                HasPermissionsToUseToolParams {
                    tool_use_id: "toolu_read_explicit_ask",
                    tool_name: "Read",
                    mcp_info: None,
                    input_summary: &outside_text,
                    input: &input,
                    context: &permissions,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: None,
                    abort_signal: None,
                },
                Some(&explicit_context),
            ),
            HasPermissionsToUseToolResult::Ask(_)
        ));
    }

    #[test]
    fn bash_permission_uses_tool_check_permissions_before_generic_prompt() {
        let context = ToolPermissionContext::default();
        let git_status = serde_json::json!({"command": "git status"});
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_git_status",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "git status",
                input: &git_status,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Allow { .. }
        ));

        let cargo_test = serde_json::json!({"command": "cargo test --quiet"});
        let request = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_bash_cargo_test",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "cargo test --quiet",
            input: &cargo_test,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected Bash permission prompt, got {other:?}"),
        };
        assert_eq!(request.rule.tool_name, "Bash");
        assert_eq!(request.rule.rule_content.as_deref(), Some("cargo test:*"));
        assert_eq!(request.suggestions.len(), 1);
        assert!(!request.is_compound_command);

        let compound = serde_json::json!({"command": "cargo test && npm test"});
        let compound_request = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_bash_compound",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "cargo test && npm test",
            input: &compound,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected compound Bash permission prompt, got {other:?}"),
        };
        assert!(compound_request.is_compound_command);
        assert_eq!(compound_request.suggestions.len(), 2);
        let response = crate::components::permissions::bash_permission_request::bash_tool_use_option_to_prompt_response(
            crate::components::permissions::bash_permission_request::bash_tool_use_options::BashToolUseOptionValue::YesApplySuggestions,
            &compound_request,
            &compound_request.suggestions,
            None,
        );
        assert_eq!(response.permission_updates, compound_request.suggestions);
        assert!(response.permission_updates_explicit);
    }

    #[test]
    fn bash_permission_integration_respects_tool_wide_rule_ordering() {
        let mut deny_context = ToolPermissionContext::default();
        deny_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        let input = serde_json::json!({"command": "git status"});
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_tool_deny",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "git status",
                input: &input,
                context: &deny_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Deny(_)
        ));

        let mut ask_context = ToolPermissionContext::default();
        ask_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_tool_ask",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "git status",
                input: &input,
                context: &ask_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        let mut allow_context = ToolPermissionContext::default();
        allow_context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        let unknown = serde_json::json!({"command": "custom-build"});
        assert_eq!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_tool_allow",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "custom-build",
                input: &unknown,
                context: &allow_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            expected_allow(
                &unknown,
                PermissionDecisionReason::Rule {
                    rule: PermissionRule {
                        source: PermissionRuleSource::Session,
                        rule_behavior: PermissionBehavior::Allow,
                        rule_value: PermissionRuleValue::new("Bash", None)
                    }
                }
            )
        );
    }

    /// CC `permissions.ts:845-875`: an unavailable classifier denies with
    /// `buildClassifierUnavailableMessage` while `tengu_iron_gate_closed` holds
    /// its default `true`, and only returns the untouched ask result when the
    /// gate is off.
    #[test]
    fn classifier_unavailable_denies_under_the_iron_gate_and_asks_without_it() {
        let request = mock_permission_request(
            "perm-toolu_unavailable",
            "toolu_unavailable",
            "Bash",
            "cargo build",
            PermissionMode::Auto,
        );
        let model = "claude-3-5-haiku-20241022";

        let denied = resolve_classifier_unavailable(
            "Bash",
            request.clone(),
            "classifier API returned 529",
            model,
            true,
        );
        let deny_req = match denied {
            HasPermissionsToUseToolResult::Deny(deny_req) => deny_req,
            other => panic!("iron gate closed must fail closed, got {other:?}"),
        };
        // Re-derived 2026-08-26 (#132): CC `permissions.ts:857-868` puts this
        // copy on the DECISION's `message`, and `PermissionDenyDecision.message`
        // is a required field (`types/permissions.ts:233`). It was asserted on
        // `description` only because `PermissionRequest` had no `message`;
        // `description` is `await tool.description(input, …)`
        // (`useCanUseTool.tsx:138-143`) and must stay untouched by a deny.
        assert_eq!(
            deny_req.message,
            crate::utils::messages::build_classifier_unavailable_message("Bash", model),
            "deny copy must be buildClassifierUnavailableMessage verbatim"
        );
        assert!(
            deny_req.description.is_empty(),
            "a deny must not overwrite tool.description(...), got {:?}",
            deny_req.description
        );

        assert_eq!(
            resolve_classifier_unavailable(
                "Bash",
                request.clone(),
                "classifier API returned 529",
                model,
                false,
            ),
            HasPermissionsToUseToolResult::Ask(request),
            "fail open returns the original ask result untouched"
        );
    }

    /// End-to-end counterpart: Auto mode with an unavailable classifier must
    /// reach `HasPermissionsToUseToolResult::Deny` through the real gate value.
    ///
    /// Fixture re-derived against CC when the `:600-656` acceptEdits fast-path
    /// landed. It used to drive `git status`, which cannot reach the classifier
    /// in CC at all: the fast-path re-runs `BashTool.checkPermissions`, and
    /// `bashPermissions.ts:1153-1163` returns
    /// `{behavior:'allow', decisionReason:{type:'other', reason:'Read-only command is allowed'}}`
    /// for any `isReadOnly` command, so CC returns the `:641-648` allow before
    /// `classifyYoloAction` at `:693`. `npm publish` is neither read-only nor an
    /// acceptEdits-allowed filesystem command, so it reaches `:845-868`, the
    /// branch this test is about.
    #[test]
    fn auto_mode_denies_the_tool_when_the_classifier_is_unavailable() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "unavailable");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let input = serde_json::json!({"command": "npm publish --access public"});
        let mut auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };
        auto_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        let decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_auto_unavailable",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "npm publish --access public",
            input: &input,
            context: &auto_context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let deny_req = match decision {
            HasPermissionsToUseToolResult::Deny(deny_req) => deny_req,
            other => panic!("an unavailable classifier must fail closed, got {other:?}"),
        };
        // Re-derived 2026-08-26 (#132): the retry guidance is the decision's
        // `message` (`permissions.ts:864-867`), not the tool's own description.
        assert!(
            deny_req
                .message
                .contains("so auto mode cannot determine the safety of Bash right now"),
            "deny carries the official retry guidance, got {:?}",
            deny_req.message
        );
        // Maps to: CC `:857-863` — the fail-closed deny also carries the
        // classifier reason, which is what `toolExecution.ts:1075-1078` keys the
        // PermissionDenied hooks on.
        assert_eq!(
            deny_req.decision_reason,
            Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: "Classifier unavailable".to_string(),
            })
        );
    }

    /// Regression: CC outer `hasPermissionsToUseTool` still runs the auto
    /// classifier when an alwaysAsk rule returned `ask`. Default mode must
    /// surface Ask; Auto + force allow must Allow without UI.
    ///
    /// Auto-mode fixture re-derived against CC alongside
    /// `auto_mode_denies_the_tool_when_the_classifier_is_unavailable`: the
    /// original `git status` returns CC's `:641-648` acceptEdits-fast-path allow
    /// (`bashPermissions.ts:1153-1163` read-only allow), so it could no longer
    /// distinguish "classifier consulted" from "fast-path fired". The default
    /// half keeps `git status` — CC step 1b (`:1184-1203`) returns the ask rule
    /// before `checkPermissions` runs at all.
    #[test]
    fn tool_wide_ask_rule_is_ask_in_default_but_classifier_in_auto() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "allow");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let read_only_input = serde_json::json!({"command": "git status"});
        let classified_input = serde_json::json!({"command": "npm publish --access public"});
        let mut ask_context = ToolPermissionContext::default();
        ask_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );

        // Default: alwaysAsk → interactive Ask (no classifier).
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_ask_default",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "git status",
                input: &read_only_input,
                context: &ask_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        // Auto: outer path routes ask through classifier (force allow).
        ask_context.mode = PermissionMode::Auto;
        crate::utils::permissions::auto_mode_state::set_auto_mode_active(true);
        assert_eq!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_ask_auto",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "npm publish --access public",
                input: &classified_input,
                context: &ask_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            expected_allow(
                &classified_input,
                PermissionDecisionReason::Classifier {
                    classifier: "auto-mode".into(),
                    reason: "forced allow for Bash".into()
                }
            )
        );

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();
    }

    /// Maps to: CC `utils/permissions/permissions.ts:658-686` — the safe-tool
    /// allowlist short-circuits BEFORE `formatActionForClassifier` /
    /// `classifyYoloAction` (`:689-693`), returning
    /// `{behavior:'allow', updatedInput: input, decisionReason:{type:'mode', mode:'auto'}}`
    /// and resetting the streak with `recordSuccess` at `:661-662`.
    /// Membership is `SAFE_YOLO_ALLOWLISTED_TOOLS`
    /// (`utils/permissions/classifierDecision.ts:56-93`).
    ///
    /// "The classifier is not consulted" is asserted on the DECISION, not by
    /// waiting for a call that must not come: `COMETIX_AUTO_CLASSIFIER_FORCE=block`
    /// makes any classifier consultation resolve to `Block` in microseconds, so a
    /// regression fails instead of hanging (`validation.md` § 4). The
    /// non-allowlisted control proves the force switch is live in this fixture.
    #[test]
    fn auto_mode_allowlisted_tool_matches_official_classifier_skip() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        // OUTSIDE the working dir on purpose: `FileReadTool.checkPermissions`
        // asks for such a path even under an `acceptEdits` overlay, so CC's
        // earlier `:600-656` fast-path cannot fire and the only remaining
        // pre-classifier producer of an allow is the `:658-686` allowlist.
        let read_path = std::env::temp_dir().join("cometix-allowlist-read.txt");
        let read_path = read_path.display().to_string();

        let run = |tool_name: &str,
                   input: &serde_json::Value,
                   input_summary: &str,
                   denials: &crate::tool::SharedDenialTracking| {
            let mut auto_context = ToolPermissionContext {
                mode: PermissionMode::Auto,
                ..ToolPermissionContext::default()
            };
            // A whole-tool ask rule is CC step 1b (`permissions.ts:1184-1203`),
            // which lands the decision in the auto branch at `:520`.
            auto_context.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new(tool_name, None)],
            );
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_allowlist",
                tool_name,
                mcp_info: None,
                input_summary,
                input,
                context: &auto_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: Some(denials.clone()),
                abort_signal: None,
            })
        };

        // CC `:661` recordSuccess: a streak is reset by the fast-path allow.
        let denials = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState {
                consecutive_denials: 2,
                total_denials: 2,
            },
        );
        let read_input = serde_json::json!({ "file_path": read_path });
        let allowed = run("Read", &read_input, &read_path, &denials);

        // Control: WebFetch is NOT in SAFE_YOLO_ALLOWLISTED_TOOLS, so CC pays for
        // the classifier — here forced to `block`.
        let control_denials = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState::default(),
        );
        let fetch_input = serde_json::json!({
            "url": "https://example.com/docs",
            "prompt": "summarize",
        });
        let control = run(
            "WebFetch",
            &fetch_input,
            "https://example.com/docs",
            &control_denials,
        );

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        assert_eq!(
            allowed,
            expected_allow(
                &read_input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::Auto
                }
            ),
            "CC :678-685 allows an allowlisted tool without consulting the classifier"
        );
        assert_eq!(
            denials.get().consecutive_denials,
            0,
            "CC :661-662 recordSuccess + persistDenialState resets the streak"
        );
        assert_eq!(
            denials.get().total_denials,
            2,
            "recordSuccess (denialTracking.ts:42-51) leaves totalDenials alone"
        );
        assert!(
            matches!(control, HasPermissionsToUseToolResult::Deny(_)),
            "control proves the forced classifier is reachable from this fixture, got {control:?}"
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:600-656` — before paying
    /// for a classifier call, re-run `tool.checkPermissions` under an
    /// `acceptEdits` overlay (`:607-619`); an `allow` returns at `:641-648`
    /// after `recordSuccess` at `:621-622`. Write is deliberately NOT on the
    /// classifier allowlist — `classifierDecision.ts:53-54` says write/edit
    /// tools "are handled by the acceptEdits fast path (allowed in CWD,
    /// classified outside CWD)", which is exactly the pair asserted here.
    #[test]
    fn auto_mode_accept_edits_fast_path_matches_official_classifier_skip() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let inside = crate::bootstrap::state::get_original_cwd().join("accept-edits-fast-path.txt");
        let inside = inside.display().to_string();
        let outside = std::env::temp_dir().join("cometix-accept-edits-outside.txt");
        let outside = outside.display().to_string();

        let run = |path: &str, denials: &crate::tool::SharedDenialTracking| {
            let auto_context = ToolPermissionContext {
                mode: PermissionMode::Auto,
                ..ToolPermissionContext::default()
            };
            let input = serde_json::json!({ "file_path": path, "content": "hello" });
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_accept_edits_fast_path",
                tool_name: "Write",
                mcp_info: None,
                input_summary: path,
                input: &input,
                context: &auto_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: Some(denials.clone()),
                abort_signal: None,
            })
        };

        let denials = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState {
                consecutive_denials: 2,
                total_denials: 2,
            },
        );
        let in_cwd = run(&inside, &denials);

        let control_denials = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState::default(),
        );
        let out_of_cwd = run(&outside, &control_denials);

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        // The fast path now carries CC `:641-648`'s payload: the acceptEdits
        // probe's updatedInput and the `mode` decision reason.
        assert!(
            matches!(
                in_cwd,
                HasPermissionsToUseToolResult::Allow {
                    decision_reason: Some(PermissionDecisionReason::Mode {
                        mode: PermissionMode::Auto
                    }),
                    ..
                }
            ),
            "CC :641-648 allows without a classifier call when acceptEdits would allow; got {in_cwd:?}"
        );
        assert_eq!(
            denials.get().consecutive_denials,
            0,
            "CC :621-622 recordSuccess + persistDenialState resets the streak"
        );
        assert_eq!(
            denials.get().total_denials,
            2,
            "recordSuccess (denialTracking.ts:42-51) leaves totalDenials alone"
        );
        assert!(
            matches!(out_of_cwd, HasPermissionsToUseToolResult::Deny(_)),
            "outside the working dir acceptEdits does not allow, so CC pays for the classifier; got {out_of_cwd:?}"
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:903-911` (block deny) and
    /// `handleDenialLimitExceeded` `:1045-1057` (denial-limit ask): both carry
    /// `decisionReason: {type:'classifier', classifier:'auto-mode', reason}`.
    ///
    /// The denial-limit branch returns `{...result, decisionReason}` — it does
    /// NOT touch `message`, so nothing overwrites the dialog's own
    /// `tool.description(...)` text; the warning is only reachable through
    /// `PermissionRuleExplanation`'s classifier arm
    /// (`PermissionRuleExplanation.tsx:37-42`).
    #[test]
    fn auto_mode_classifier_block_matches_official_decision_reason() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let input = serde_json::json!({"command": "npm publish --access public"});
        let run = |denials: &crate::tool::SharedDenialTracking| {
            let mut auto_context = ToolPermissionContext {
                mode: PermissionMode::Auto,
                ..ToolPermissionContext::default()
            };
            auto_context.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![PermissionRuleValue::new("Bash", None)],
            );
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_classifier_block",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "npm publish --access public",
                input: &input,
                context: &auto_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: Some(denials.clone()),
                abort_signal: None,
            })
        };

        let fresh = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState::default(),
        );
        let blocked = run(&fresh);

        // consecutive_denials 2 + this denial == DENIAL_LIMITS.max_consecutive.
        let at_limit = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState {
                consecutive_denials: 2,
                total_denials: 2,
            },
        );
        let limit_reached = run(&at_limit);

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let HasPermissionsToUseToolResult::Deny(deny) = blocked else {
            panic!("a classifier block denies, got {blocked:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: "forced block for Bash".to_string(),
            }),
            "CC :905-909"
        );
        assert!(
            !deny.description.starts_with("[ClassifierBlocked]"),
            "the invented prefix existed only because the reason was dropped, got {:?}",
            deny.description
        );
        // Maps to: CC `permissions.ts:910` `message: buildYoloRejectionMessage(
        // classifierResult.reason)` — the copy the MODEL receives through
        // `toolExecution.ts:1023`. Before #132 the raw reason landed on
        // `description` (the tool's self-description) and this copy did not
        // exist in the port at all.
        assert_eq!(
            deny.message,
            crate::utils::messages::build_yolo_rejection_message("forced block for Bash")
        );
        assert!(
            deny.description.is_empty(),
            "a deny must not overwrite tool.description(...), got {:?}",
            deny.description
        );

        let HasPermissionsToUseToolResult::Ask(ask) = limit_reached else {
            panic!("the denial limit falls back to prompting, got {limit_reached:?}");
        };
        assert_eq!(
            ask.decision_reason,
            Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: "3 consecutive actions were blocked. Please review the transcript before continuing.\n\nLatest blocked action: forced block for Bash".to_string(),
            }),
            "CC :1050-1057"
        );
        assert!(
            ask.description.is_empty(),
            "CC `{{...result}}` leaves the dialog description to tool.description(...), got {:?}",
            ask.description
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:572-591` — with the ant-only
    /// `POWERSHELL_AUTO_MODE` build flag off (this port's external profile),
    /// PowerShell returns the untouched `ask` instead of reaching the
    /// acceptEdits fast-path or the classifier, and denies with an `asyncAgent`
    /// reason when prompts are unavailable (`:576-586`).
    #[test]
    fn auto_mode_powershell_matches_official_explicit_permission_guard() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "allow");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let input = serde_json::json!({ "command": "Get-ChildItem -Recurse" });
        let run = |should_avoid_permission_prompts: bool| {
            let mut auto_context = ToolPermissionContext {
                mode: PermissionMode::Auto,
                should_avoid_permission_prompts,
                ..ToolPermissionContext::default()
            };
            auto_context.always_ask_rules.insert(
                PermissionRuleSource::Session,
                vec![
                    PermissionRuleValue::new("PowerShell", None),
                    // CC powershellPermissions.ts:694-711,902-907: this ask
                    // survives the acceptEdits probe even with a working AST.
                    PermissionRuleValue::new("PowerShell", Some("Get-ChildItem:*".into())),
                ],
            );
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_powershell_auto",
                tool_name: "PowerShell",
                mcp_info: None,
                input_summary: "Get-ChildItem -Recurse",
                input: &input,
                context: &auto_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            })
        };

        let interactive = run(false);
        let headless = run(true);

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::Permissions,
        ) {
            // CC permissions.ts:572-591,600-648,918-926: internal permits
            // classification; an explicit content ask prevents the acceptEdits
            // fast-path, so both interactive and headless use the classifier.
            for result in [interactive, headless] {
                let HasPermissionsToUseToolResult::Allow {
                    updated_input,
                    decision_reason,
                    ..
                } = result
                else {
                    panic!("internal profile classifies PowerShell in auto mode, got {result:?}");
                };
                assert_eq!(updated_input, Some(input.clone()));
                assert_eq!(
                    decision_reason,
                    Some(PermissionDecisionReason::Classifier {
                        classifier: "auto-mode".into(),
                        reason: "forced allow for PowerShell".into(),
                    })
                );
            }
            return;
        }

        assert!(
            matches!(interactive, HasPermissionsToUseToolResult::Ask(_)),
            "CC :590 returns the untouched ask rather than the forced classifier allow, got {interactive:?}"
        );
        let HasPermissionsToUseToolResult::Deny(deny) = headless else {
            panic!("CC :577-585 denies when prompts are unavailable, got {headless:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "PowerShell tool requires interactive approval and permission prompts are not available in this context".to_string(),
            })
        );
        // Re-derived 2026-08-26 (#132): CC `:577-585` spells this on the
        // decision's `message` (`message: 'PowerShell tool requires interactive
        // approval'`), which is required on `PermissionDenyDecision`
        // (`types/permissions.ts:233`). The port asserted `description` only
        // because that field did not exist yet.
        assert_eq!(
            deny.message,
            "PowerShell tool requires interactive approval"
        );
        assert!(
            deny.description.is_empty(),
            "a deny must not overwrite tool.description(...), got {:?}",
            deny.description
        );
    }

    /// Maps to: CC `permissions.ts:929-952` — the NON-auto ask tail. A
    /// headless (shouldAvoidPermissionPrompts) ask in default mode, with no
    /// PermissionRequest hooks configured, auto-denies with the `asyncAgent`
    /// reason and `AUTO_REJECT_MESSAGE(tool.name)`. Before #145 the ask
    /// escaped the permission system untouched and the resolver layer denied
    /// with the ask's own `createPermissionRequestMessage` copy instead.
    #[test]
    fn headless_non_auto_ask_denies_with_async_agent_reason_and_auto_reject_message() {
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/denied.txt", "content": "x"});
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_headless",
            tool_name: "Write",
            mcp_info: None,
            input_summary: "/tmp/denied.txt",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :944-951 auto-denies the headless ask, got {result:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "Permission prompts are not available in this context".to_string(),
            })
        );
        assert_eq!(
            deny.message,
            crate::utils::messages::auto_reject_message("Write"),
            "the model-facing copy is AUTO_REJECT_MESSAGE, not the ask's own message"
        );
    }

    /// Install a single `PermissionRequest` command hook for the headless-tail
    /// tests. `capture_hooks_config_snapshot` is CC's own
    /// `captureHooksConfigSnapshot`, and `load_hooks_config()` reads it back
    /// before the settings sources, so the hook reaches the tail without
    /// touching the on-disk settings this process shares.
    fn capture_permission_request_hook(command: &str) {
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &crate::utils::settings::SettingsJson {
                hooks: Some(serde_json::json!({
                    "PermissionRequest": [{
                        "matcher": "Write",
                        "hooks": [{"type": "command", "command": command}],
                    }],
                })),
                ..Default::default()
            },
            None,
            false,
        );
    }

    /// The other half of [`capture_permission_request_hook`]: pin the settings
    /// snapshot EMPTY so the only `PermissionRequest` matcher in play is the one
    /// the test registers on the session. Without this the tail would read
    /// whatever hooks this repository's own settings carry, and a session-hook
    /// test could pass on a settings hook's decision.
    fn capture_no_settings_hooks() {
        crate::utils::hooks::hooks_config_snapshot::capture_hooks_config_snapshot(
            &crate::utils::settings::SettingsJson::default(),
            None,
            false,
        );
        crate::bootstrap::state::clear_registered_hooks();
    }

    /// A `PermissionRequest` command hook that denies with `message`, in the
    /// shape `registerFrontmatterHooks` installs (CC `runAgent.ts:568`).
    fn session_deny_hook(message: &str) -> crate::services::hooks::HookCommand {
        crate::services::hooks::HookCommand {
            command: format!(
                r#"printf '%s\n' '{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"deny","message":"{message}"}}}}}}'"#
            ),
            shell: None,
            timeout: Some(5),
            condition: None,
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        }
    }

    fn headless_write_params<'a>(
        input: &'a serde_json::Value,
        context: &'a ToolPermissionContext,
    ) -> HasPermissionsToUseToolParams<'a> {
        HasPermissionsToUseToolParams {
            tool_use_id: "toolu_headless_hook",
            tool_name: "Write",
            mcp_info: None,
            input_summary: "/tmp/hooked.txt",
            input,
            context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }
    }

    /// Maps to: CC `permissions.ts:422-443` — the headless hook ALLOW arm does
    /// two things with `decision.updatedPermissions`: `persistPermissionUpdates`
    /// AND `context.setAppState(prev => ({...prev, toolPermissionContext:
    /// applyPermissionUpdates(prev.toolPermissionContext, updates)}))`. The port
    /// only persisted, so a `session`-destination update — which persists to
    /// nothing by design — evaporated entirely and the rest of the turn ran
    /// under the old permission context.
    #[tokio::test]
    async fn headless_hook_allow_projects_updated_permissions_into_live_app_state() {
        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_permission_request_hook(
            r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedPermissions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}]}}}'"#,
        );

        // CC permissions.ts:506,929 reads headlessness from getAppState,
        // not from a separate permission snapshot supplied to the call.
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(context),
                ..Default::default()
            },
            None,
        );
        assert_eq!(
            store.get().tool_permission_context.mode,
            PermissionMode::Default
        );
        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_app_store(store.clone());

        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &tool_use_context.tool_permission_context),
            Some(&tool_use_context),
        )
        .await;

        let HasPermissionsToUseToolResult::Allow { updated_input, .. } = result else {
            panic!("CC :434-442 allows on a hook allow decision, got {result:?}");
        };
        assert_eq!(
            updated_input,
            Some(input.clone()),
            "CC `decision.updatedInput ?? input` keeps the original when the hook sends none"
        );
        assert_eq!(
            store.get().tool_permission_context.mode,
            PermissionMode::AcceptEdits,
            "CC :428-433 projects the hook's updates into the live AppState"
        );
    }

    /// Maps to: CC `permissions.ts:445-458` — the headless hook DENY arm aborts
    /// the turn's controller when `decision.interrupt` is truthy, and returns
    /// `decision.message || 'Permission denied by hook'` with the same message
    /// as the hook `decisionReason.reason`.
    ///
    /// Both bits used to be unreachable: the message was read off
    /// `hook_permission_decision_reason`, which belongs to the PreToolUse arm
    /// and is stripped from a PermissionRequest hook's output by the schema, and
    /// nothing read `interrupt` at all.
    #[tokio::test]
    async fn headless_hook_deny_with_interrupt_aborts_and_keeps_the_hook_message() {
        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_permission_request_hook(
            r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"blocked by policy","interrupt":true}}}'"#,
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let abort_controller = tool_use_context.abort_controller.clone();
        assert!(!abort_controller.is_aborted());

        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        )
        .await;

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :451-458 denies on a hook deny decision, got {result:?}");
        };
        assert_eq!(deny.message, "blocked by policy");
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: Some("blocked by policy".to_string()),
            })
        );
        assert!(
            abort_controller.is_aborted(),
            "CC :445-450 aborts the controller before returning the deny"
        );
    }

    /// The negative half of the same CC line: `interrupt` is
    /// `z.boolean().optional()` (`types/hooks.ts:131`) and CC truthy-tests it,
    /// so an omitted flag denies WITHOUT touching the controller. An absent
    /// `message` falls back to `'Permission denied by hook'` (`:452`) while the
    /// `decisionReason.reason` stays the raw `undefined` (`:456`).
    #[tokio::test]
    async fn headless_hook_deny_without_interrupt_leaves_the_controller_alone() {
        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_permission_request_hook(
            r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny"}}}'"#,
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let abort_controller = tool_use_context.abort_controller.clone();

        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        )
        .await;

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :451-458 denies on a hook deny decision, got {result:?}");
        };
        assert_eq!(deny.message, "Permission denied by hook");
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: None,
            })
        );
        assert!(
            !abort_controller.is_aborted(),
            "an omitted interrupt must never abort the turn"
        );
    }

    /// Maps to: CC `permissions.ts:409-412` — the headless loop skips any hook
    /// result without a `permissionRequestResult` and switches on
    /// `decision.behavior`. A top-level `{"decision":"approve"}` sets
    /// `HookResult.permissionBehavior` (`hooks.ts:512-514`) but NOT
    /// `permissionRequestResult`, so CC ignores it and the ask still reaches the
    /// auto-deny. Matching on the flattened behavior instead let any hook with
    /// that two-key output silently allow a headless tool call.
    #[tokio::test]
    async fn headless_top_level_approve_is_not_a_permission_request_decision() {
        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_permission_request_hook(r#"printf '%s\n' '{"decision":"approve"}'"#);

        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            None,
        )
        .await;

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :944-951 still auto-denies, got {result:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "Permission prompts are not available in this context".to_string(),
            })
        );
    }

    /// CC assembles this tail's hook set through
    /// `runPermissionRequestHooksForHeadlessAgent` → `executePermissionRequestHooks`
    /// → `executeHooks` → `getMatchingHooks(appState, toolUseContext?.agentId ??
    /// getSessionId(), …)` → `getHooksConfig` (`permissions.ts:409-415`,
    /// `hooks.ts:2001-2010`, `:1491-1565`), which merges `appState.sessionHooks`
    /// for that session on top of the settings snapshot and the registered
    /// hooks. A background agent's own frontmatter registrations (CC
    /// `runAgent.ts:568` `registerFrontmatterHooks`, keyed by `agentId`)
    /// therefore decide the very asks that agent raises — and this tail is the
    /// only permission path a background agent ever takes, because
    /// `shouldAvoidPermissionPrompts` is what routes it here.
    ///
    /// Old shape (verified by reverting the merge, 2026-08-29):
    /// `resolve_headless_ask_owned` read `load_hooks_config()` alone — the two
    /// process-level sources — so with the settings snapshot pinned empty the
    /// `has_hooks` guard skipped hook execution outright and the ask fell to
    /// CC's `:944-951` auto-deny. That is a FAILURE, not a hang: unlike the SDK
    /// race this tail always produces a decision, so the assertions below are
    /// what report it and no timeout is needed.
    #[tokio::test]
    async fn headless_ask_runs_the_agents_own_session_registered_hooks() {
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks;

        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_no_settings_hooks();
        session_hooks::clear_all_session_hooks();
        // Registered under the AGENT id: CC's session id for a subagent's own
        // hooks is `toolUseContext.agentId` (`hooks.ts:2003`).
        session_hooks::add_session_hook(
            "agent-headless-7",
            HookEvent::PermissionRequest,
            "Write",
            session_deny_hook("denied by agent frontmatter hook"),
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_agent_id(Some("agent-headless-7".to_string()));
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        )
        .await;
        session_hooks::clear_all_session_hooks();

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :451-458 denies on a hook deny decision, got {result:?}");
        };
        assert_eq!(
            deny.message, "denied by agent frontmatter hook",
            "an agent's own registered hook must decide its headless ask, not be skipped"
        );
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: Some("denied by agent frontmatter hook".to_string()),
            })
        );
    }

    /// The same merge, for the MAIN session: CC's `?? getSessionId()` fallback
    /// (`hooks.ts:2003`) means a hook the main session registered also runs on a
    /// headless ask raised with no `agentId` — which is what a `--print` run and
    /// `registerSkillHooks` produce. Old shape: skipped for the same reason as
    /// the agent case, and equally a failure rather than a hang.
    #[tokio::test]
    async fn headless_ask_runs_main_session_registered_hooks_without_an_agent_id() {
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks;

        // The guard clears the hooks-config snapshot on BOTH edges, so the
        // capture has to come after the lock and needs no manual teardown.
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_no_settings_hooks();
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            &crate::bootstrap::state::get_session_id(),
            HookEvent::PermissionRequest,
            "Write",
            session_deny_hook("denied by main-session hook"),
        );

        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        // No `ToolUseContext` at all: the `agentId` projection is `None`, which
        // is exactly the arm `?? getSessionId()` covers.
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            None,
        )
        .await;
        session_hooks::clear_all_session_hooks();

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :451-458 denies on a hook deny decision, got {result:?}");
        };
        assert_eq!(
            deny.message, "denied by main-session hook",
            "the `?? getSessionId()` arm must merge too"
        );
    }

    /// Installs a managed policy file for the lifetime of the returned guards
    /// and pins the settings cache to it.
    ///
    /// `CLAUDE_CONFIG_DIR` (pinned by `just test`) does NOT cover the managed
    /// settings root, so a test that means to assert on policy behaviour has to
    /// redirect `CLAUDE_CODE_MANAGED_SETTINGS_PATH` itself. Requires
    /// `TEST_ENV_LOCK`. Trust is untouched: this relocates the POLICY root, not
    /// the config root or the cwd, so the harness's own trusted workspace still
    /// answers `check_has_trust_dialog_accepted()` — which is what lets these
    /// tests tell the managed arm of `should_skip_hook_execution` apart from
    /// its trust arm.
    fn managed_policy_root(contents: &str) -> (std::path::PathBuf, EnvVarGuard) {
        let root =
            std::env::temp_dir().join(format!("cometix-headless-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create managed root");
        std::fs::write(root.join("managed-settings.json"), contents)
            .expect("write policy settings");
        let guard = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
        // A direct disk write bypasses production invalidation, so the test
        // states the invariant itself (same note as `settings/mod.rs` tests).
        crate::utils::settings::settings_cache::reset_settings_cache();
        (root, guard)
    }

    /// Maps to: CC `hooks.ts:1978-1980` — `executeHooks` returns before doing
    /// anything when `shouldDisableAllHooksIncludingManaged()`, i.e. when POLICY
    /// settings carry `disableAllHooks`. That is an EXECUTION-time gate; CC's
    /// merge-time assembly (`getHooksConfig`, `:1491-1565`) never consults it.
    ///
    /// The outcome assertion alone cannot tell those two layers apart, and for
    /// one batch it did not have to: `resolve_headless_ask_owned` carried its
    /// own `!loaded.disable_all_hooks` copy of the policy at the MERGE site
    /// (a772a9d), because when that landed no port executor had the gate. It
    /// passed identically with and without that conjunct — a redundancy and a
    /// regression looked the same from here. So the assertions below name the
    /// layer instead of the outcome:
    ///
    /// - the merge-time filter is INERT (`allow_managed_hooks_only == false`),
    ///   so it is provably not what stopped the hook;
    /// - the execution-time gate FIRES, and fires on its managed arm rather than
    ///   its trust arm.
    ///
    /// Old shape: with the shared entry removed from `tool::execute_hooks`, the
    /// `should_skip_hook_execution` assertion fails and the deny below carries
    /// the hook's own message instead of the auto-deny reason.
    #[tokio::test]
    async fn headless_ask_skips_session_hooks_under_a_disable_all_hooks_policy() {
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let (managed_root, _managed_guard) = managed_policy_root(r#"{"disableAllHooks": true}"#);
        capture_no_settings_hooks();
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-headless-policy",
            HookEvent::PermissionRequest,
            "Write",
            session_deny_hook("denied by agent frontmatter hook"),
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_agent_id(Some("agent-headless-policy".to_string()));
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        )
        .await;

        // WHICH layer stopped it — read while the policy is still installed.
        let loaded = crate::services::hooks::load_hooks_config();
        assert!(
            loaded.disable_all_hooks,
            "precondition: the managed policy is in force"
        );
        assert!(
            !loaded.allow_managed_hooks_only,
            "CC's merge-time filter (`hooks.ts:1516`, `:1541`) is inert under \
             `disableAllHooks`, so the session arm IS merged and the merge site \
             cannot be what stopped the hook"
        );
        assert!(
            crate::services::hooks::should_disable_all_hooks_including_managed_from_settings(),
            "the managed arm of the execution gate is the one that fires"
        );
        assert!(
            crate::services::hooks::should_skip_hook_execution(
                HookEvent::PermissionRequest,
                "Write",
            ),
            "CC `hooks.ts:1978-1980` at execution entry is what stops it, reached \
             here through `permission_request.rs:88` -> `tool.rs:169`"
        );

        session_hooks::clear_all_session_hooks();
        let _ = std::fs::remove_dir_all(&managed_root);

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :944-951 auto-denies with no hook decision, got {result:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "Permission prompts are not available in this context".to_string(),
            }),
            "a disableAllHooks policy must stop the session hook before it can decide"
        );
    }

    /// The complementary layer, and the reason the merge site keeps its SECOND
    /// conjunct after the first one was dropped as redundant.
    ///
    /// Maps to: CC `hooks.ts:1534-1541` — "Skip session hooks entirely when
    /// allowManagedHooksOnly is set — this prevents frontmatter hooks from
    /// agents/skills from bypassing the policy". Unlike `disableAllHooks`, this
    /// policy has NO execution-time twin: `executeHooks` never checks it as a
    /// gate (it reads it only for telemetry, `:2080-2082`), because it is a
    /// per-SOURCE filter — managed hooks still run. So an agent's own session
    /// hooks can only be stopped where they are merged.
    ///
    /// Old shape (verified 2026-08-30 by dropping `!loaded.allow_managed_hooks_only`
    /// too): the session hook merges and decides the ask, so the deny carries
    /// `PermissionDecisionReason::Hook` with "denied by agent frontmatter hook"
    /// instead of the auto-deny reason — a failed assertion, not a hang.
    #[tokio::test]
    async fn headless_ask_skips_session_hooks_under_an_allow_managed_hooks_only_policy() {
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let (managed_root, _managed_guard) =
            managed_policy_root(r#"{"allowManagedHooksOnly": true}"#);
        capture_no_settings_hooks();
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-headless-managed-only",
            HookEvent::PermissionRequest,
            "Write",
            session_deny_hook("denied by agent frontmatter hook"),
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_agent_id(Some("agent-headless-managed-only".to_string()));
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_async_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        )
        .await;

        // The mirror image of the `disableAllHooks` test: here the MERGE filter
        // fires and the execution gate does not.
        let loaded = crate::services::hooks::load_hooks_config();
        assert!(
            loaded.allow_managed_hooks_only,
            "precondition: the managed-only policy is in force"
        );
        assert!(
            !loaded.disable_all_hooks,
            "managed hooks still run under this policy — it is a filter, not an off switch"
        );
        assert!(
            !crate::services::hooks::should_skip_hook_execution(
                HookEvent::PermissionRequest,
                "Write",
            ),
            "the execution gate does NOT cover this policy, which is why the \
             merge site has to: `executeHooks` reads `shouldAllowManagedHooksOnly()` \
             only for telemetry (`hooks.ts:2080-2082`)"
        );

        session_hooks::clear_all_session_hooks();
        let _ = std::fs::remove_dir_all(&managed_root);

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :944-951 auto-denies with no hook decision, got {result:?}");
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "Permission prompts are not available in this context".to_string(),
            }),
            "an allowManagedHooksOnly policy must stop the session hook at the merge"
        );
    }

    /// The sync bridge is a second entry into the same tail
    /// ([`resolve_headless_ask_sync`]), and it takes its own copy of every value
    /// the future needs — so the `agentId` projection has to be threaded there
    /// too or the production `check_can_use_tool` path keeps the old behaviour
    /// while the async twin is fixed. Same CC anchors as the async test above.
    #[test]
    fn headless_ask_sync_bridge_runs_the_agents_own_session_registered_hooks() {
        use crate::services::hooks::HookEvent;
        use crate::utils::hooks::session_hooks;

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        capture_no_settings_hooks();
        session_hooks::clear_all_session_hooks();
        session_hooks::add_session_hook(
            "agent-headless-sync",
            HookEvent::PermissionRequest,
            "Write",
            session_deny_hook("denied by agent frontmatter hook"),
        );

        let tool_use_context =
            crate::tool::ToolUseContext::with_permission_context(ToolPermissionContext::default())
                .with_agent_id(Some("agent-headless-sync".to_string()));
        let context = ToolPermissionContext {
            mode: PermissionMode::Default,
            should_avoid_permission_prompts: true,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({"file_path": "/tmp/hooked.txt", "content": "x"});
        let result = has_permissions_to_use_tool_with_context(
            headless_write_params(&input, &context),
            Some(&tool_use_context),
        );
        session_hooks::clear_all_session_hooks();

        let HasPermissionsToUseToolResult::Deny(deny) = result else {
            panic!("CC :451-458 denies on a hook deny decision, got {result:?}");
        };
        assert_eq!(
            deny.message, "denied by agent frontmatter hook",
            "the sync bridge must key the hook set by the same agentId"
        );
    }

    /// Maps to: CC `permissions.ts:1183-1206` — the whole-tool Bash ask rule
    /// carries the matched rule as its `decisionReason`. The
    /// `canSandboxAutoAllow` exception (`:1189-1195`) must NOT fire here:
    /// sandboxing is disabled in this environment, so the rule still asks.
    /// If the exception's polarity were inverted, this ask would fall through
    /// to `BashTool.checkPermissions` and lose the rule reason.
    #[test]
    fn bash_whole_tool_ask_rule_asks_with_rule_reason_when_sandbox_auto_allow_is_off() {
        let mut context = ToolPermissionContext::default();
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        let input = serde_json::json!({"command": "echo hello"});
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_bash_ask_rule",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "echo hello",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let HasPermissionsToUseToolResult::Ask(ask) = result else {
            panic!("a whole-tool ask rule must ask, got {result:?}");
        };
        assert!(
            matches!(
                ask.decision_reason,
                Some(PermissionDecisionReason::Rule { ref rule })
                    if rule.rule_value.tool_name == "Bash"
            ),
            "the ask carries the MATCHED whole-tool rule (CC :1198-1201), got {:?}",
            ask.decision_reason
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:1244-1250` (step 1f) read
    /// together with `:505-548`. Step 1f returns a content-specific ask rule from
    /// `hasPermissionsToUseToolInner`, which makes it immune to step 2a's
    /// bypassPermissions allow (`:1272-1281`) — and to nothing else. The outer
    /// entry still runs `if (result.behavior === 'ask')` at `:505` on that value,
    /// and `:532-534` exempts only a `safetyCheck`, so a `{type:'rule'}` reason
    /// falls straight through `:549-686` to `classifyYoloAction` at `:689-693`.
    ///
    /// "The classifier IS consulted" is asserted on the decision rather than on a
    /// call count: `COMETIX_AUTO_CLASSIFIER_FORCE=block` resolves any
    /// consultation to `Block` in microseconds, and CC `:903-911` stamps that
    /// deny with `{type:'classifier', classifier:'auto-mode', reason}` — a value
    /// no other branch in this function can produce.
    #[test]
    fn auto_mode_content_ask_rule_matches_official_classifier_fallthrough() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        // A CONTENT ask rule, not a whole-tool one: `BashTool.checkPermissions`
        // matches it and returns `{behavior:'ask', decisionReason:{type:'rule',
        // rule:{ruleBehavior:'ask'}}}` (`bash_permissions.rs:673-681`), which is
        // exactly the shape step 1f keys on. A whole-tool rule would be step 1b
        // (`:1184-1203`) and is already covered elsewhere.
        let mut auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };
        auto_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("npm publish:*".to_string()),
            )],
        );
        let input = serde_json::json!({ "command": "npm publish --access public" });
        let decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_content_ask_auto",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "npm publish --access public",
            input: &input,
            context: &auto_context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let HasPermissionsToUseToolResult::Deny(deny) = decision else {
            panic!(
                "CC :532-534 exempts only a safetyCheck, so a content ask rule reaches \
                 classifyYoloAction at :689-693; got {decision:?}"
            );
        };
        assert_eq!(
            deny.decision_reason,
            Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: "forced block for Bash".to_string(),
            }),
            "CC :903-911 — the classifier's own deny reason proves the classifier ran"
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:532-548` and its comment at
    /// `:526-531`, which splits one `safetyCheck` reason into two auto-mode
    /// outcomes on `classifierApprovable`:
    ///
    /// - `true` (sensitive-file paths, `filesystem.ts:643-660`) "fall through to
    ///   the classifier";
    /// - `false` (`filesystem.ts:632-638` Windows path bypass attempts) "stay
    ///   immune to ALL auto-approve paths", and deny instead of asking when
    ///   `shouldAvoidPermissionPrompts` is set (`:536-546`).
    ///
    /// Step 1g (`:1255-1260`) never reads the flag; this is its only reader.
    /// Both arms are driven through `WriteTool` so the flag comes from the real
    /// producer chain `check_path_safety_for_auto_edit → filesystem.rs:1279-1288`.
    #[test]
    fn auto_mode_safety_check_matches_official_classifier_approvable_split() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let cwd = crate::bootstrap::state::get_original_cwd();
        // `.git` is in DANGEROUS_DIRECTORIES → `classifierApprovable: true`
        // (CC `filesystem.ts:652-660`).
        let approvable_path = cwd.join(".git/cometix-parity-probe").display().to_string();
        // `PROGRA~1` is an 8.3 short name → `hasSuspiciousWindowsPathPattern` →
        // `classifierApprovable: false` (CC `filesystem.ts:630-639`).
        let non_approvable_path = cwd
            .join("PROGRA~1/cometix-parity-probe")
            .display()
            .to_string();

        let run = |path: &str, should_avoid_permission_prompts: bool| {
            let auto_context = ToolPermissionContext {
                mode: PermissionMode::Auto,
                should_avoid_permission_prompts,
                ..ToolPermissionContext::default()
            };
            let input = serde_json::json!({ "file_path": path, "content": "probe" });
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_safety_check_auto",
                tool_name: "Write",
                mcp_info: None,
                input_summary: path,
                input: &input,
                context: &auto_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            })
        };

        let approvable = run(&approvable_path, false);
        let non_approvable = run(&non_approvable_path, false);
        let non_approvable_headless = run(&non_approvable_path, true);

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let HasPermissionsToUseToolResult::Deny(classified) = approvable else {
            panic!(
                "CC :529-531 — a classifierApprovable safetyCheck falls through to the \
                 classifier; got {approvable:?}"
            );
        };
        assert_eq!(
            classified.decision_reason,
            Some(PermissionDecisionReason::Classifier {
                classifier: "auto-mode".to_string(),
                reason: "forced block for Write".to_string(),
            }),
            "CC :903-911 — the classifier's own deny reason proves the classifier ran"
        );

        let HasPermissionsToUseToolResult::Ask(asked) = non_approvable else {
            panic!("CC :547 returns the untouched ask; got {non_approvable:?}");
        };
        assert!(
            matches!(
                asked.decision_reason,
                Some(PermissionDecisionReason::SafetyCheck {
                    classifier_approvable: false,
                    ..
                })
            ),
            "CC :547 returns `result` verbatim, safetyCheck reason included; got {:?}",
            asked.decision_reason
        );

        let HasPermissionsToUseToolResult::Deny(denied) = non_approvable_headless else {
            panic!(
                "CC :537-545 denies when prompts are unavailable; got {non_approvable_headless:?}"
            );
        };
        assert_eq!(
            denied.decision_reason,
            Some(PermissionDecisionReason::AsyncAgent {
                reason: "Safety check requires interactive approval and permission prompts are not available in this context".to_string(),
            }),
            "CC :540-544"
        );
        // CC `:539` is `message: result.message` — the ask's own text survives the
        // switch to a deny; only `decisionReason` is replaced.
        assert_eq!(denied.message, asked.message, "CC :539");
    }

    /// Maps to: CC `utils/permissions/permissions.ts:1244-1250` — step 1f exists
    /// so a user-configured content ask rule survives bypassPermissions mode,
    /// "just as deny rules are respected at step 1d" (`:1238-1243`). The ask
    /// returns from the inner at `:1249`, which is above step 2a's allow at
    /// `:1272-1281`.
    ///
    /// This is the half the auto-mode fix must not regress: routing the immune
    /// ask into the outer branch changes what AUTO does with it and nothing else.
    #[test]
    fn bypass_permissions_content_ask_rule_matches_official_step_1f_immunity() {
        let mut bypass_context = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        let input = serde_json::json!({ "command": "npm publish --access public" });
        let run = |context: &ToolPermissionContext| {
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_content_ask_bypass",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "npm publish --access public",
                input: &input,
                context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            })
        };

        // Control first: without the rule, step 2a's `shouldBypassPermissions`
        // allow (`:1268-1281`) is live in this fixture.
        assert_eq!(
            run(&bypass_context),
            expected_allow(
                &input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::BypassPermissions
                }
            ),
            "CC :1272-1281 allows in bypassPermissions mode"
        );

        bypass_context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("npm publish:*".to_string()),
            )],
        );
        let decision = run(&bypass_context);
        let HasPermissionsToUseToolResult::Ask(asked) = decision else {
            panic!("CC :1244-1250 returns the ask above step 2a's allow; got {decision:?}");
        };
        assert!(
            matches!(
                asked.decision_reason,
                Some(PermissionDecisionReason::Rule { ref rule })
                    if rule.rule_behavior == PermissionBehavior::Ask
            ),
            "CC :1246-1247 keys on `decisionReason.rule.ruleBehavior === 'ask'`; got {:?}",
            asked.decision_reason
        );
    }

    #[test]
    fn bash_permission_integration_respects_deny_rules_and_security_checks() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", Some("rm:*".to_string()))],
        );
        let denied = serde_json::json!({"command": "FOO=a=b rm -rf target"});
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_rm",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "FOO=a=b rm -rf target",
                input: &denied,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Deny(_)
        ));

        let dangerous = serde_json::json!({"command": "echo $(id)"});
        let request = match has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_bash_subst",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "echo $(id)",
            input: &dangerous,
            context: &ToolPermissionContext::default(),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        }) {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected ask for dangerous Bash command, got {other:?}"),
        };
        assert_eq!(request.rule.rule_content.as_deref(), Some("echo $(id)"));

        let dont_ask = ToolPermissionContext {
            mode: PermissionMode::DontAsk,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_bash_subst_dont_ask",
                tool_name: "Bash",
                mcp_info: None,
                input_summary: "echo $(id)",
                input: &dangerous,
                context: &dont_ask,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Deny(_)
        ));
    }

    /// A tool's `checkPermissions` allow payload must survive the gate. CC
    /// returns the tool's allow AS the gate's result (`permissions.ts:486-500`),
    /// so `updatedInput` reaches execution (`getUpdatedInputOrFallback`); the
    /// port's unit `Allow` used to flatten it away.
    ///
    /// `SyntheticOutput.checkPermissions` returns
    /// `{behavior:'allow', updatedInput: args}` unconditionally
    /// (`synthetic_output_tool/mod.rs:108`), which makes it the cleanest
    /// end-to-end carrier: the gate result must carry the SAME input back.
    #[test]
    fn tool_allow_updated_input_survives_the_gate() {
        let context = ToolPermissionContext::default();
        let input = serde_json::json!({ "output": "final answer" });
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_synthetic",
            tool_name: "StructuredOutput",
            mcp_info: None,
            input_summary: "structured output",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        let HasPermissionsToUseToolResult::Allow { updated_input, .. } = result else {
            panic!("SyntheticOutput must be allowed, got {result:?}");
        };
        assert_eq!(
            updated_input.as_ref(),
            Some(&input),
            "the tool's updatedInput must ride the gate result"
        );
    }

    #[test]
    fn web_fetch_permission_uses_official_domain_rule_and_preapproved_hosts() {
        let context = ToolPermissionContext::default();
        let preapproved_input = serde_json::json!({
            "url": "https://doc.rust-lang.org/book/",
            "prompt": "summarize"
        });
        let preapproved = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_docs",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://doc.rust-lang.org/book/",
            input: &preapproved_input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(matches!(
            preapproved,
            HasPermissionsToUseToolResult::Allow { .. }
        ));

        let mut whole_tool_denied_context = ToolPermissionContext::default();
        whole_tool_denied_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("WebFetch", None)],
        );
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_webfetch_docs_denied",
                tool_name: "WebFetch",
                mcp_info: None,
                input_summary: "https://doc.rust-lang.org/book/",
                input: &preapproved_input,
                context: &whole_tool_denied_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Deny(_)
        ));

        let mut domain_denied_context = ToolPermissionContext::default();
        domain_denied_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "WebFetch",
                Some("domain:doc.rust-lang.org".to_string()),
            )],
        );
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu_webfetch_docs_domain_denied",
                tool_name: "WebFetch",
                mcp_info: None,
                input_summary: "https://doc.rust-lang.org/book/",
                input: &preapproved_input,
                context: &domain_denied_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Allow { .. }
        ));

        let unapproved = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_example",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://example.com/docs/page",
            input: &serde_json::json!({
                "url": "https://example.com/docs/page",
                "prompt": "summarize"
            }),
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        let request = match unapproved {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected ask for unapproved WebFetch URL, got {other:?}"),
        };
        assert_eq!(request.rule.tool_name, "WebFetch");
        assert_eq!(
            request.rule.rule_content.as_deref(),
            Some("domain:example.com")
        );

        let (allowed_context, _) = apply_prompt_choice(
            &ToolPermissionContext::default(),
            &request,
            PermissionPromptChoice::AlwaysAllow,
        );
        let same_domain = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_example_2",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://example.com/other",
            input: &serde_json::json!({
                "url": "https://example.com/other",
                "prompt": "summarize"
            }),
            context: &allowed_context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(matches!(
            same_domain,
            HasPermissionsToUseToolResult::Allow { .. }
        ));
    }

    /// The outer gate routes WebFetch through `WebFetchTool.checkPermissions`
    /// (CC step 1c) instead of a bespoke branch, so its domain rules decide and
    /// `toAutoClassifierInput` reaches the auto-mode classifier. While the gate
    /// forced `tool_call = None` for WebFetch the tool never ran, and an
    /// unapproved domain in Auto mode took the empty-projection allow.
    #[test]
    fn web_fetch_routes_through_the_tool_check_permissions_and_the_auto_classifier() {
        let unapproved_input = serde_json::json!({
            "url": "https://example.com/docs/page",
            "prompt": "summarize"
        });

        let mut domain_denied_context = ToolPermissionContext::default();
        domain_denied_context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "WebFetch",
                Some("domain:example.com".to_string()),
            )],
        );
        assert!(
            matches!(
                has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                    tool_use_id: "toolu_webfetch_domain_denied",
                    tool_name: "WebFetch",
                    mcp_info: None,
                    input_summary: "https://example.com/docs/page",
                    input: &unapproved_input,
                    context: &domain_denied_context,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: None,
                    abort_signal: None,
                }),
                HasPermissionsToUseToolResult::Deny(_)
            ),
            "domain-scoped deny must come from WebFetchTool.checkPermissions"
        );

        let ask = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_unapproved",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://example.com/docs/page",
            input: &unapproved_input,
            context: &ToolPermissionContext::default(),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        let request = match ask {
            HasPermissionsToUseToolResult::Ask(request) => request,
            other => panic!("expected ask for unapproved WebFetch URL, got {other:?}"),
        };
        assert_eq!(
            request.suggestions,
            vec![PermissionUpdate::AddRules {
                destination: crate::types::permissions::PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new(
                    "WebFetch",
                    Some("domain:example.com".to_string())
                )],
            }],
            "only WebFetchTool.checkPermissions builds these suggestions"
        );

        assert_eq!(
            projected_classifier_input("WebFetch", &unapproved_input),
            Some(serde_json::json!(
                "https://example.com/docs/page: summarize"
            )),
            "an empty projection is what let Auto mode skip the classifier"
        );

        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1");
        crate::utils::process_env::set("COMETIX_AUTO_CLASSIFIER_FORCE", "block");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        let auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };
        let auto_decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_auto",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://example.com/docs/page",
            input: &unapproved_input,
            context: &auto_context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        crate::utils::process_env::remove("COMETIX_AUTO_CLASSIFIER_FORCE");
        crate::utils::process_env::remove("COMETIX_TRANSCRIPT_CLASSIFIER");
        crate::utils::permissions::auto_mode_state::reset_for_testing();

        assert!(
            matches!(auto_decision, HasPermissionsToUseToolResult::Deny(_)),
            "unapproved domain in Auto mode must reach the classifier, got {auto_decision:?}"
        );
    }

    #[test]
    fn write_uses_edit_permission_identity_for_tool_wide_deny_rules() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Edit", None)],
        );
        let input = serde_json::json!({"file_path": "new-write.txt", "content": "hello"});
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu-write-deny",
            tool_name: "Write",
            mcp_info: None,
            input_summary: "new-write.txt",
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        let HasPermissionsToUseToolResult::Deny(request) = result else {
            panic!("tool-wide Edit deny must block Write")
        };
        assert_eq!(request.rule.tool_name, "Edit");
    }

    #[test]
    fn write_permission_uses_invocation_cwd_override_in_generic_gate() {
        let invocation_cwd = std::env::temp_dir().join(format!(
            "cometix-write-permission-cwd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut permission_context = ToolPermissionContext::default();
        permission_context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Edit", Some("src/**".to_string()))],
        );
        let mut tool_context =
            crate::tool::ToolUseContext::with_permission_context(permission_context);
        tool_context.cwd_override = Some(invocation_cwd.clone());
        let input = serde_json::json!({
            "file_path": invocation_cwd.join("src/new.rs"),
            "content": "hello"
        });
        let params = || HasPermissionsToUseToolParams {
            tool_use_id: "toolu-write-cwd",
            tool_name: "Write",
            mcp_info: None,
            input_summary: input["file_path"].as_str().unwrap(),
            input: &input,
            context: &tool_context.tool_permission_context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        };

        assert!(matches!(
            has_permissions_to_use_tool(params()),
            HasPermissionsToUseToolResult::Ask(_)
        ));
        assert!(matches!(
            has_permissions_to_use_tool_with_context(params(), Some(&tool_context)),
            HasPermissionsToUseToolResult::Allow { .. }
        ));
    }

    #[test]
    fn write_permission_retains_filesystem_suggestions_in_generic_gate() {
        let path = crate::bootstrap::state::get_original_cwd().join("new-write.txt");
        let path = path.display().to_string();
        let input = serde_json::json!({"file_path": path, "content": "hello"});
        let context = ToolPermissionContext::default();
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu-write-permission",
            tool_name: "Write",
            mcp_info: None,
            input_summary: input["file_path"].as_str().unwrap(),
            input: &input,
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        let HasPermissionsToUseToolResult::Ask(request) = result else {
            panic!("default Write should ask")
        };
        assert!(request.suggestions.iter().any(|update| matches!(
            update,
            PermissionUpdate::SetMode {
                destination: PermissionUpdateDestination::Session,
                mode: PermissionMode::AcceptEdits,
            }
        )));
    }

    #[test]
    fn edit_ordinary_filesystem_ask_is_overridden_by_bypass_modes() {
        let path = crate::bootstrap::state::get_original_cwd().join("bypass-edit.txt");
        let path = path.display().to_string();
        let input = serde_json::json!({
            "file_path": path,
            "old_string": "old",
            "new_string": "new"
        });
        let run = |context: &ToolPermissionContext| {
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu-edit-bypass",
                tool_name: "Edit",
                mcp_info: None,
                input_summary: input["file_path"].as_str().unwrap(),
                input: &input,
                context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            })
        };

        let mut bypass = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        assert_eq!(
            run(&bypass),
            expected_allow(&input, PermissionDecisionReason::Mode { mode: bypass.mode })
        );

        bypass.mode = PermissionMode::Plan;
        bypass.is_bypass_permissions_mode_available = true;
        assert_eq!(
            run(&bypass),
            expected_allow(&input, PermissionDecisionReason::Mode { mode: bypass.mode })
        );
    }

    #[test]
    fn notebook_edit_ordinary_filesystem_ask_is_overridden_by_bypass_mode() {
        let path = crate::bootstrap::state::get_original_cwd().join("bypass-notebook.ipynb");
        let path = path.display().to_string();
        let input = serde_json::json!({
            "notebook_path": path,
            "cell_id": "cell-a",
            "new_source": "new"
        });
        let context = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        assert_eq!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu-notebook-bypass",
                tool_name: "NotebookEdit",
                mcp_info: None,
                input_summary: input["notebook_path"].as_str().unwrap(),
                input: &input,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            expected_allow(
                &input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::BypassPermissions
                }
            )
        );
    }

    #[test]
    fn edit_explicit_ask_rule_and_safety_check_remain_bypass_immune() {
        let cwd = crate::bootstrap::state::get_original_cwd();
        let ordinary_path = cwd.join("explicit-ask-edit.txt").display().to_string();
        let mut context = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Edit",
                Some(format!("/{ordinary_path}")),
            )],
        );
        let ordinary_input = serde_json::json!({
            "file_path": ordinary_path,
            "old_string": "old",
            "new_string": "new"
        });
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu-edit-explicit-ask",
                tool_name: "Edit",
                mcp_info: None,
                input_summary: ordinary_input["file_path"].as_str().unwrap(),
                input: &ordinary_input,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        context.always_ask_rules.clear();
        let safety_path = cwd.join(".git/config").display().to_string();
        let safety_input = serde_json::json!({
            "file_path": safety_path,
            "old_string": "old",
            "new_string": "new"
        });
        assert!(matches!(
            has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu-edit-safety-ask",
                tool_name: "Edit",
                mcp_info: None,
                input_summary: safety_input["file_path"].as_str().unwrap(),
                input: &safety_input,
                context: &context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            }),
            HasPermissionsToUseToolResult::Ask(_)
        ));
    }

    #[test]
    fn permission_decision_deny_records_rejection_without_rule_update() {
        let request = mock_permission_request(
            "p1",
            "toolu_1",
            "Bash",
            "rm -rf target",
            PermissionMode::Default,
        );
        let (ctx, decision) = apply_prompt_choice(
            &ToolPermissionContext::default(),
            &request,
            PermissionPromptChoice::Deny,
        );

        assert_eq!(decision.behavior, PermissionBehavior::Deny);
        assert!(decision.transcript.is_empty());
        assert!(ctx.always_allow_rules.is_empty());
    }

    #[test]
    fn permission_decision_always_allow_updates_in_memory_rule() {
        let request = mock_permission_request(
            "p1",
            "toolu_1",
            "Bash",
            "cargo check",
            PermissionMode::Default,
        );
        let (ctx, decision) = apply_prompt_choice(
            &ToolPermissionContext::default(),
            &request,
            PermissionPromptChoice::AlwaysAllow,
        );

        assert_eq!(decision.behavior, PermissionBehavior::Allow);
        assert!(has_in_memory_allow_rule(&ctx, &request.rule));
    }

    #[test]
    fn dont_ask_mode_transforms_prompt_into_deny_after_allow_fast_paths() {
        let mut context = ToolPermissionContext::default();
        context.mode = PermissionMode::DontAsk;

        let bash_result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_bash",
            tool_name: "Bash",
            mcp_info: None,
            input_summary: "rm -rf target",
            input: &serde_json::json!({ "command": "rm -rf target" }),
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(matches!(
            bash_result,
            HasPermissionsToUseToolResult::Deny(_)
        ));

        let read_result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_read",
            tool_name: "Read",
            mcp_info: None,
            input_summary: "Cargo.toml",
            input: &serde_json::json!({ "file_path": "Cargo.toml" }),
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(matches!(
            read_result,
            HasPermissionsToUseToolResult::Allow { .. }
        ));
    }

    /// Maps to: CC `utils/permissions/permissions.ts:508-517` — the dontAsk
    /// ask→deny transform returns a FRESH decision:
    ///
    /// ```ts
    /// return {
    ///   behavior: 'deny',
    ///   decisionReason: { type: 'mode', mode: 'dontAsk' },
    ///   message: DONT_ASK_REJECT_MESSAGE(tool.name),
    /// }
    /// ```
    ///
    /// Both halves are replaced; the originating ask's `decisionReason` does not
    /// survive. The port previously forwarded it, so a whole-tool ask rule's
    /// `{type:'rule'}` leaked into the deny.
    #[test]
    fn dont_ask_deny_matches_official_mode_decision_reason() {
        let mut context = ToolPermissionContext {
            mode: PermissionMode::DontAsk,
            ..ToolPermissionContext::default()
        };
        // A whole-tool ASK rule, so the ask this deny replaces carries a
        // `{type:'rule'}` reason of its own (`permissions.ts:1197`).
        context.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", None)],
        );

        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_read_dont_ask_reason",
            tool_name: "Read",
            mcp_info: None,
            input_summary: "Cargo.toml",
            input: &serde_json::json!({ "file_path": "Cargo.toml" }),
            context: &context,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let HasPermissionsToUseToolResult::Deny(request) = result else {
            panic!("dontAsk must convert the ask into a deny: {result:?}");
        };
        assert_eq!(
            request.decision_reason,
            Some(PermissionDecisionReason::Mode {
                mode: PermissionMode::DontAsk
            }),
            "CC replaces the whole decision at permissions.ts:509-516, so the \
             originating ask's rule reason must not survive",
        );
        assert_eq!(
            request.message,
            crate::utils::messages::dont_ask_reject_message("Read"),
        );
    }

    #[test]
    fn dont_ask_runs_after_whole_tool_allow_matches_official_order() {
        let input = serde_json::json!({"file_path": "/tmp/.../secret.txt"});
        let mut context = ToolPermissionContext {
            mode: PermissionMode::DontAsk,
            ..ToolPermissionContext::default()
        };
        let tool_context = crate::tool::ToolUseContext::with_permission_context(context.clone());
        let decide = |permission_context: &ToolPermissionContext,
                      tool_context: &crate::tool::ToolUseContext| {
            has_permissions_to_use_tool_with_context(
                HasPermissionsToUseToolParams {
                    tool_use_id: "toolu_read_dont_ask_order",
                    tool_name: "Read",
                    mcp_info: None,
                    input_summary: "/tmp/.../secret.txt",
                    input: &input,
                    context: permission_context,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: None,
                    abort_signal: None,
                },
                Some(tool_context),
            )
        };

        assert!(matches!(
            decide(&context, &tool_context),
            HasPermissionsToUseToolResult::Deny(_)
        ));

        context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Read", None)],
        );
        let tool_context = crate::tool::ToolUseContext::with_permission_context(context.clone());
        assert_eq!(
            decide(&context, &tool_context),
            expected_allow(
                &input,
                PermissionDecisionReason::Rule {
                    rule: PermissionRule {
                        source: PermissionRuleSource::Session,
                        rule_behavior: PermissionBehavior::Allow,
                        rule_value: PermissionRuleValue::new("Read", None)
                    }
                }
            ),
            "CC applies the whole-tool allow before the outer dontAsk conversion"
        );
    }

    #[test]
    fn requires_user_interaction_ask_precedes_bypass_and_whole_tool_allow() {
        let input = serde_json::json!({});
        let decide = |context: &ToolPermissionContext| {
            let tool_context =
                crate::tool::ToolUseContext::with_permission_context(context.clone());
            has_permissions_to_use_tool_with_context(
                HasPermissionsToUseToolParams {
                    tool_use_id: "toolu_exit_plan_interaction",
                    tool_name: "ExitPlanMode",
                    mcp_info: None,
                    input_summary: "",
                    input: &input,
                    context,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: None,
                    abort_signal: None,
                },
                Some(&tool_context),
            )
        };

        let bypass = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            decide(&bypass),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        let mut whole_tool_allowed = ToolPermissionContext::default();
        whole_tool_allowed.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("ExitPlanMode", None)],
        );
        assert!(matches!(
            decide(&whole_tool_allowed),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        let dont_ask = ToolPermissionContext {
            mode: PermissionMode::DontAsk,
            ..ToolPermissionContext::default()
        };
        assert!(matches!(
            decide(&dont_ask),
            HasPermissionsToUseToolResult::Deny(_)
        ));
    }

    /// Decide a single tool use through the full-context entry, the way the
    /// dispatcher does. Keeps the per-tool parity cases below to one line each.
    fn decide_tool(
        tool_name: &str,
        input: &serde_json::Value,
        context: &ToolPermissionContext,
    ) -> HasPermissionsToUseToolResult {
        let tool_context = crate::tool::ToolUseContext::with_permission_context(context.clone());
        has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_tool_allow_parity",
                tool_name,
                mcp_info: None,
                input_summary: "",
                input,
                context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&tool_context),
        )
    }

    /// Maps to: CC `permissions.ts:1299-1310` (step 3 rewrites ONLY `passthrough`
    /// into `ask`) + `permissions.ts:486-501` (the outer entry returns `allow`
    /// before the dontAsk transform and the auto-mode classifier).
    /// `AgentTool.tsx:1692-1708`: the auto-mode passthrough branch is behind
    /// `USER_TYPE === 'ant'`, so an external build's only live branch is
    /// `{behavior:'allow'}` — CC never prompts for a subagent spawn.
    #[test]
    fn agent_spawn_is_allowed_without_a_prompt_matches_official() {
        let input = serde_json::json!({
            "description": "audit permissions",
            "prompt": "Check the permission pipeline.",
        });

        for mode in [PermissionMode::Default, PermissionMode::Auto] {
            let context = ToolPermissionContext {
                mode,
                ..ToolPermissionContext::default()
            };
            let decision = decide_tool(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                &input,
                &context,
            );
            if crate::utils::build_profile::has_internal_capability(
                crate::utils::build_profile::InternalCapability::Permissions,
            ) && mode == PermissionMode::Auto
            {
                // The ant-only passthrough branch; it reaches the classifier.
                assert!(
                    !matches!(decision, HasPermissionsToUseToolResult::Allow { .. }),
                    "internal builds keep the auto-mode passthrough"
                );
            } else {
                assert!(
                    matches!(decision, HasPermissionsToUseToolResult::Allow { .. }),
                    "AgentTool.checkPermissions returns allow in {mode:?}; CC returns it verbatim"
                );
            }
        }

        // The tool's allow is the LAST word, not the first: CC still evaluates
        // whole-tool deny (1a) and ask (1b) rules before calling the tool.
        let mut denied = ToolPermissionContext::default();
        denied.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                None,
            )],
        );
        assert!(matches!(
            decide_tool(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                &input,
                &denied
            ),
            HasPermissionsToUseToolResult::Deny(_)
        ));

        let mut asked = ToolPermissionContext::default();
        asked.always_ask_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                None,
            )],
        );
        assert!(matches!(
            decide_tool(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                &input,
                &asked
            ),
            HasPermissionsToUseToolResult::Ask(_)
        ));
    }

    /// Every other tool whose own `check_permissions` result used to be dropped
    /// on the floor. Each row cites the CC owner of the expected behavior.
    #[test]
    fn tool_check_permissions_result_is_final_matches_official() {
        let context = ToolPermissionContext::default();

        // CC has no `checkPermissions` on these tools, so `TOOL_DEFAULTS`
        // (Tool.ts:757-769) resolves them to allow. Grep evidence: the only CC
        // files defining `checkPermissions` are the 22 under `src/tools/` listed
        // in `has_permissions_to_use_tool_inner`'s doc comment; none is these.
        for (tool_name, input) in [
            ("TaskOutput", serde_json::json!({ "task_id": "t1" })),
            ("TaskStop", serde_json::json!({ "task_id": "t1" })),
            ("EnterWorktree", serde_json::json!({})),
            ("ExitWorktree", serde_json::json!({ "action": "keep" })),
            (
                "RemoteTrigger",
                // Not just the read actions: CC allows every action here.
                serde_json::json!({ "action": "create", "body": {} }),
            ),
            (
                "CronCreate",
                serde_json::json!({ "cron": "*/5 * * * *", "prompt": "ping" }),
            ),
        ] {
            assert!(
                matches!(
                    decide_tool(tool_name, &input, &context),
                    HasPermissionsToUseToolResult::Allow { .. }
                ),
                "{tool_name} inherits CC's TOOL_DEFAULTS allow"
            );
        }

        // Maps to: CC `SyntheticOutputTool.ts:66-72` — explicit allow. This tool
        // never appears in `get_all_base_tools()`, so it also pins that a missing
        // schema row does not suppress the tool's own decision.
        assert!(matches!(
            decide_tool(
                crate::tools::synthetic_output_tool::SYNTHETIC_OUTPUT_TOOL_NAME,
                &serde_json::json!({ "answer": 42 }),
                &context
            ),
            HasPermissionsToUseToolResult::Allow { .. }
        ));

        // Maps to: CC `ConfigTool.ts:98-107` — reads allow, writes ask.
        assert!(matches!(
            decide_tool(
                "Config",
                &serde_json::json!({ "setting": "theme" }),
                &context
            ),
            HasPermissionsToUseToolResult::Allow { .. }
        ));
        assert!(matches!(
            decide_tool(
                "Config",
                &serde_json::json!({ "setting": "theme", "value": "dark" }),
                &context
            ),
            HasPermissionsToUseToolResult::Ask(_)
        ));

        // Fail-closed side of the same change: a tool that returns `passthrough`
        // still prompts. Maps to CC `MCPTool.ts:56-61` / `WebSearchTool.ts:209`.
        for (tool_name, input) in [
            (
                crate::tools::mcp_tool::prompt::MCP_TOOL_NAME,
                serde_json::json!({}),
            ),
            ("WebSearch", serde_json::json!({ "query": "rust iocraft" })),
        ] {
            assert!(
                matches!(
                    decide_tool(tool_name, &input, &context),
                    HasPermissionsToUseToolResult::Ask(_)
                ),
                "{tool_name} returns passthrough; CC step 3 converts it to ask"
            );
        }
    }

    /// Permission-side half of #144: CC parses with `tool.inputSchema.parse`
    /// immediately before `tool.checkPermissions` (`permissions.ts:1215-1216`),
    /// and Agent's plain `z.object` STRIPS unknown fields there. The
    /// JSON-Schema fallback would return `None` for the same input
    /// (`additionalProperties: false`), suppressing the tool's own decision.
    #[test]
    fn agent_permission_parse_strips_unknown_fields_instead_of_failing() {
        static AGENT: crate::tools::agent_tool::AgentTool = crate::tools::agent_tool::AgentTool;
        let parsed = parse_tool_permission_input(
            &AGENT,
            "Agent",
            &serde_json::json!({
                "description": "run tests",
                "prompt": "run the suite",
                "made_up_field": true,
            }),
        )
        .expect("a plain z.object accepts unknown fields");
        assert_eq!(parsed.get("made_up_field"), None, "unknown field stripped");
        assert_eq!(
            parsed.get("prompt"),
            Some(&serde_json::json!("run the suite"))
        );

        // A missing required field is still CC's `parse` throw.
        assert_eq!(
            parse_tool_permission_input(
                &AGENT,
                "Agent",
                &serde_json::json!({"description": "no prompt"}),
            ),
            None
        );
    }

    #[test]
    fn agent_deny_rules_filter_specific_agent_types() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct AgentStub {
            agent_type: &'static str,
        }

        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
                Some("Explore".to_string()),
            )],
        );

        let deny = get_deny_rule_for_agent(
            &context,
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
            "Explore",
        )
        .expect("deny rule");
        assert_eq!(deny.source, PermissionRuleSource::Session);
        assert_eq!(deny.rule_value.rule_content.as_deref(), Some("Explore"));

        let filtered = filter_denied_agents(
            &[
                AgentStub {
                    agent_type: "Explore",
                },
                AgentStub { agent_type: "Plan" },
            ],
            &context,
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
            |agent| agent.agent_type,
        );
        assert_eq!(filtered, vec![AgentStub { agent_type: "Plan" }]);
    }

    #[test]
    fn canonical_rule_iteration_covers_all_sources_independent_of_hashmap_insertion() {
        let mut first = ToolPermissionContext::default();
        let mut second = ToolPermissionContext::default();
        for source in PERMISSION_RULE_SOURCES.iter().rev() {
            first.always_allow_rules.insert(
                *source,
                vec![PermissionRuleValue::new(format!("Tool{source:?}"), None)],
            );
        }
        for source in PERMISSION_RULE_SOURCES {
            second.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(format!("Tool{source:?}"), None)],
            );
        }

        let first_rules = get_allow_rules(&first);
        let second_rules = get_allow_rules(&second);
        assert_eq!(first_rules, second_rules);
        assert_eq!(
            first_rules
                .iter()
                .map(|rule| rule.source)
                .collect::<Vec<_>>(),
            PERMISSION_RULE_SOURCES
        );
    }

    #[test]
    fn delete_permission_rule_matches_official_read_only_source_guard() {
        for source in [
            PermissionRuleSource::PolicySettings,
            PermissionRuleSource::FlagSettings,
            PermissionRuleSource::Command,
        ] {
            let rule = PermissionRule {
                source,
                rule_behavior: PermissionBehavior::Allow,
                rule_value: PermissionRuleValue::new("Read", None),
            };
            assert_eq!(
                delete_permission_rule(&ToolPermissionContext::default(), &rule)
                    .unwrap_err()
                    .to_string(),
                "Cannot delete permission rules from read-only settings"
            );
        }

        let rule = PermissionRule {
            source: PermissionRuleSource::Session,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("Read", None),
        };
        let mut context = ToolPermissionContext::default();
        context
            .always_allow_rules
            .insert(PermissionRuleSource::Session, vec![rule.rule_value.clone()]);
        let updated = delete_permission_rule(&context, &rule).unwrap();
        assert!(updated.always_allow_rules[&PermissionRuleSource::Session].is_empty());
    }

    #[test]
    fn duplicate_rules_keep_first_whole_tool_source_and_later_content_metadata() {
        let mut context = ToolPermissionContext::default();
        for source in PERMISSION_RULE_SOURCES {
            context.always_allow_rules.insert(
                source,
                vec![
                    PermissionRuleValue::new("Read", None),
                    PermissionRuleValue::new("Read", Some("/secret/**".to_string())),
                ],
            );
        }

        let whole =
            tool_always_allowed_rule(&context, &PermissionRuleValue::new("Read", None), None)
                .expect("whole-tool rule");
        assert_eq!(whole.source, PermissionRuleSource::UserSettings);

        let content =
            get_rule_by_contents_for_tool_name(&context, "Read", PermissionBehavior::Allow);
        assert_eq!(content.len(), 1);
        assert_eq!(
            content["/secret/**"].source,
            PermissionRuleSource::Session,
            "later duplicate replaces metadata without adding a second key"
        );
    }

    #[test]
    fn tool_rule_matching_covers_mcp_server_level_and_wildcard_rules() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![
                PermissionRuleValue::new("mcp__untrusted-server", None),
                PermissionRuleValue::new("mcp__wildcard-server__*", None),
                PermissionRuleValue::new("mcp__scoped-server__only-this", None),
            ],
        );

        for tool_name in [
            "mcp__untrusted-server__tool1",
            "mcp__untrusted-server__nested__tool",
            "mcp__untrusted-server",
            "mcp__wildcard-server__anything",
            "mcp__scoped-server__only-this",
        ] {
            assert!(
                get_deny_rule_for_tool(&context, &PermissionRuleValue::new(tool_name, None), None)
                    .is_some(),
                "{tool_name} should be denied"
            );
        }

        for tool_name in [
            "mcp__scoped-server__other",
            "mcp__other-server__tool1",
            "Bash",
        ] {
            assert!(
                get_deny_rule_for_tool(&context, &PermissionRuleValue::new(tool_name, None), None)
                    .is_none(),
                "{tool_name} should not be denied"
            );
        }
    }

    /// Maps to: CC `toolMatchesRule` resolving through
    /// `getToolNameForPermissionCheck`, which is what keeps an unprefixed SDK
    /// MCP tool (`CLAUDE_AGENT_SDK_MCP_NO_PREFIX`) from answering to the rules
    /// written for the builtin whose name it borrows.
    #[test]
    fn unprefixed_mcp_tool_and_builtin_do_not_share_rule_match_names() {
        let mcp_info = McpToolInfo {
            server_name: "srv".to_string(),
            tool_name: "Write".to_string(),
        };
        let requested = PermissionRuleValue::new("Write", None);

        let mut builtin_denied = ToolPermissionContext::default();
        builtin_denied.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new("Write", None)],
        );
        assert!(
            get_deny_rule_for_tool(&builtin_denied, &requested, Some(&mcp_info)).is_none(),
            "rule `Write` must not reach the MCP tool that merely displays as Write"
        );
        assert!(
            get_deny_rule_for_tool(&builtin_denied, &requested, None).is_some(),
            "rule `Write` still denies the builtin"
        );

        let mut mcp_denied = ToolPermissionContext::default();
        mcp_denied.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new("mcp__srv__Write", None)],
        );
        assert!(
            get_deny_rule_for_tool(&mcp_denied, &requested, Some(&mcp_info)).is_some(),
            "the MCP tool is reachable through its qualified name"
        );
        assert!(
            get_deny_rule_for_tool(&mcp_denied, &requested, None).is_none(),
            "rule `mcp__srv__Write` must not reach the builtin Write"
        );
    }

    #[test]
    fn mcp_info_rule_matching_keeps_server_level_and_wildcard_semantics() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![
                PermissionRuleValue::new("mcp__server1", None),
                PermissionRuleValue::new("mcp__wildcard__*", None),
            ],
        );

        let server1_tool1 = McpToolInfo {
            server_name: "server1".to_string(),
            tool_name: "tool1".to_string(),
        };
        let wildcard_any = McpToolInfo {
            server_name: "wildcard".to_string(),
            tool_name: "anything".to_string(),
        };
        // A distinct server whose name merely starts with `server1`.
        let server12_tool1 = McpToolInfo {
            server_name: "server12".to_string(),
            tool_name: "tool1".to_string(),
        };

        for (info, requested_name) in [
            (&server1_tool1, "mcp__server1__tool1"),
            (&wildcard_any, "mcp__wildcard__anything"),
        ] {
            let requested = PermissionRuleValue::new(requested_name, None);
            assert!(
                get_deny_rule_for_tool(&context, &requested, Some(info)).is_some(),
                "{requested_name} should be denied through mcp_info"
            );
            assert!(
                get_deny_rule_for_tool(&context, &requested, None).is_some(),
                "{requested_name} should be denied through its qualified name"
            );
        }

        let requested = PermissionRuleValue::new("mcp__server12__tool1", None);
        assert!(
            get_deny_rule_for_tool(&context, &requested, Some(&server12_tool1)).is_none(),
            "rule `mcp__server1` must not match server `server12` by string prefix"
        );
        assert!(get_deny_rule_for_tool(&context, &requested, None).is_none());
    }

    #[test]
    fn tool_rule_matching_ignores_rules_that_carry_content() {
        let mut context = ToolPermissionContext::default();
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![
                PermissionRuleValue::new("mcp__server", Some("arg".to_string())),
                PermissionRuleValue::new("Bash", Some(String::new())),
            ],
        );

        assert!(
            get_deny_rule_for_tool(
                &context,
                &PermissionRuleValue::new("mcp__server__tool", None),
                None
            )
            .is_none()
        );
        assert!(
            get_deny_rule_for_tool(&context, &PermissionRuleValue::new("Bash", None), None)
                .is_none()
        );
    }

    #[test]
    fn managed_reload_matches_official_literal_clear_and_preservation_matrix() {
        let mut context = ToolPermissionContext::default();
        for source in PERMISSION_RULE_SOURCES {
            context.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(format!("Old{source:?}"), None)],
            );
        }
        let fresh_policy = PermissionRule {
            source: PermissionRuleSource::PolicySettings,
            rule_behavior: PermissionBehavior::Allow,
            rule_value: PermissionRuleValue::new("FreshPolicy", None),
        };
        let managed =
            sync_permission_rules_from_disk(&context, std::slice::from_ref(&fresh_policy), true);

        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::ProjectSettings,
            PermissionRuleSource::LocalSettings,
            PermissionRuleSource::CliArg,
            PermissionRuleSource::Session,
        ] {
            assert!(managed.always_allow_rules[&source].is_empty(), "{source:?}");
        }
        assert_eq!(
            managed.always_allow_rules[&PermissionRuleSource::Command][0].tool_name,
            "OldCommand"
        );
        assert_eq!(
            managed.always_allow_rules[&PermissionRuleSource::FlagSettings][0].tool_name,
            "OldFlagSettings"
        );
        assert_eq!(
            managed.always_allow_rules[&PermissionRuleSource::PolicySettings][0].tool_name,
            "FreshPolicy"
        );

        let normal =
            sync_permission_rules_from_disk(&context, std::slice::from_ref(&fresh_policy), false);
        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::ProjectSettings,
            PermissionRuleSource::LocalSettings,
        ] {
            assert!(normal.always_allow_rules[&source].is_empty(), "{source:?}");
        }
        for source in [
            PermissionRuleSource::CliArg,
            PermissionRuleSource::Command,
            PermissionRuleSource::Session,
            PermissionRuleSource::FlagSettings,
        ] {
            assert_eq!(normal.always_allow_rules[&source].len(), 1, "{source:?}");
        }
    }

    /// Seeded `AppState.denialTracking` for the outer-allow reset tests.
    fn store_with_denial_tracking(
        state: crate::utils::permissions::denial_tracking::DenialTrackingState,
    ) -> crate::state::store::AppStore {
        let app_state = crate::state::app_state_store::AppState {
            denial_tracking: Some(state),
            ..Default::default()
        };
        crate::state::store::AppStore::new(app_state, None)
    }

    /// Turns `feature('TRANSCRIPT_CLASSIFIER')` on for the body and off again.
    /// The flag table already defaults it on (`utils/feature_flags.rs:306-312`);
    /// the env pin keeps these tests independent of that default.
    struct TranscriptClassifierGuard {
        _env: EnvVarGuard,
    }

    impl TranscriptClassifierGuard {
        fn on() -> Self {
            Self {
                _env: EnvVarGuard::set("COMETIX_TRANSCRIPT_CLASSIFIER", "1"),
            }
        }
    }

    /// Maps to: CC `utils/permissions/permissions.ts:483-501` — the allow branch
    /// of the OUTER `hasPermissionsToUseTool`:
    ///
    /// > Reset consecutive denials on any allowed tool use in auto mode. This
    /// > ensures that a successful tool use (even one auto-allowed by rules)
    /// > breaks the consecutive denial streak.
    ///
    /// The path here is the one CC's parenthetical names and the one task #121
    /// widened: the tool's own `checkPermissions` returns `allow`
    /// (`permissions.ts:1215-1216` → step 3 at `:1299-1310` leaves `allow`
    /// verbatim), so the classifier under `:505` never runs and its own
    /// `recordSuccess` (`:914-916`) never fires.
    #[test]
    fn auto_mode_tool_allow_matches_official_denial_streak_reset() {
        use crate::utils::permissions::denial_tracking::DenialTrackingState;

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _classifier = TranscriptClassifierGuard::on();

        // TodoWrite's own `check_permissions` returns Allow unconditionally
        // (`tools/todo_write_tool/mod.rs:219-229`). The fixture is pinned to the
        // tool's own schema assertion (`mod.rs:406-417`: item requires
        // content/status/activeForm, no `priority`/`id`) so it passes CC's
        // `inputSchema.parse` instead of landing on the passthrough seed.
        let input = serde_json::json!({
            "todos": [{
                "content": "ship it",
                "status": "pending",
                "activeForm": "Shipping it"
            }]
        });
        let seeded = DenialTrackingState {
            consecutive_denials: 2,
            total_denials: 5,
        };

        let auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };
        let store = store_with_denial_tracking(seeded);
        let decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_todowrite_auto_allow",
            tool_name: "TodoWrite",
            mcp_info: None,
            input_summary: "1 item",
            input: &input,
            context: &auto_context,
            messages: &[],
            app_store: Some(&store),
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(
            matches!(decision, HasPermissionsToUseToolResult::Allow { .. }),
            "TodoWrite.checkPermissions owns this allow; the classifier must not run"
        );
        // CC `permissions.ts:496-497`: `recordSuccess` then `persistDenialState`.
        // `recordSuccess` clears only `consecutiveDenials` (`denialTracking.ts:32-38`).
        assert_eq!(
            store.get().denial_tracking,
            Some(DenialTrackingState {
                consecutive_denials: 0,
                total_denials: 5,
            }),
            "auto-mode allow must break the consecutive denial streak and keep totalDenials"
        );

        // CC `permissions.ts:491-495`: the reset is gated on
        // `appState.toolPermissionContext.mode === 'auto'` (`:492`) — no other
        // mode resets.
        let default_context = ToolPermissionContext::default();
        assert_eq!(default_context.mode, PermissionMode::Default);
        let default_store = store_with_denial_tracking(seeded);
        let default_decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_todowrite_default_allow",
            tool_name: "TodoWrite",
            mcp_info: None,
            input_summary: "1 item",
            input: &input,
            context: &default_context,
            messages: &[],
            app_store: Some(&default_store),
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert!(matches!(
            default_decision,
            HasPermissionsToUseToolResult::Allow { .. }
        ));
        assert_eq!(
            default_store.get().denial_tracking,
            Some(seeded),
            "outside auto mode CC leaves denialTracking untouched"
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:483-501`, the "even one
    /// auto-allowed by rules" half of the same comment. Step 2b
    /// (`toolAlwaysAllowedRule`, `:1283-1297`) returns `allow` from the inner
    /// function, so the outer branch is the only place the streak can reset.
    #[test]
    fn auto_mode_rule_allow_matches_official_denial_streak_reset() {
        use crate::utils::permissions::denial_tracking::DenialTrackingState;

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _classifier = TranscriptClassifierGuard::on();

        // Same unapproved-domain WebFetch input the classifier test above uses;
        // WebFetchTool.checkPermissions returns `ask` for it, so only the
        // whole-tool allow rule can produce the allow.
        let input = serde_json::json!({
            "url": "https://example.com/docs/page",
            "prompt": "summarize"
        });
        let mut auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };
        auto_context.always_allow_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("WebFetch", None)],
        );
        let store = store_with_denial_tracking(DenialTrackingState {
            consecutive_denials: 1,
            total_denials: 1,
        });

        let decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_webfetch_rule_allow",
            tool_name: "WebFetch",
            mcp_info: None,
            input_summary: "https://example.com/docs/page",
            input: &input,
            context: &auto_context,
            messages: &[],
            app_store: Some(&store),
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert_eq!(
            decision,
            expected_allow(
                &input,
                PermissionDecisionReason::Rule {
                    rule: PermissionRule {
                        source: PermissionRuleSource::Session,
                        rule_behavior: PermissionBehavior::Allow,
                        rule_value: PermissionRuleValue::new("WebFetch", None)
                    }
                }
            )
        );
        assert_eq!(
            store.get().denial_tracking,
            Some(DenialTrackingState {
                consecutive_denials: 0,
                total_denials: 1,
            }),
            "a whole-tool allow rule breaks the streak exactly like a tool-owned allow"
        );
    }

    /// Maps to: CC `utils/permissions/permissions.ts:489-490`
    /// (`context.localDenialTracking ?? appState.denialTracking`) and
    /// `:958-978#persistDenialState` ("For async subagents with
    /// localDenialTracking, mutate the local state in place (since setAppState
    /// is a no-op). Otherwise, write to appState as usual."). `??` is nullish,
    /// so a present local state wins even when its counters differ from
    /// appState's — and the parent's state must not be written.
    /// An isolated subagent's denials must not reach the parent's store.
    ///
    /// CC gives such a fork its own `createDenialTrackingState()`
    /// (`utils/forkedAgent.ts:420-422`), and `persistDenialState`
    /// (`utils/permissions/permissions.ts:966-971`) writes locally with
    /// `Object.assign` whenever that is set — `setAppState` is the ELSE branch.
    ///
    /// This port populated the fork correctly but every permission call site
    /// hardcoded `local_denial_tracking: None`, so the writes fell through to
    /// `app_store` — and `forked_agent.rs:79` hands the child the PARENT's
    /// store. A child that kept getting denied pushed the parent session toward
    /// `should_fallback_to_prompting`. `writable = false` (`:102`) does not
    /// protect it: the permission path takes `&AppStore`, not the handle that
    /// carries the flag.
    #[test]
    fn isolated_subagent_denials_stay_local_matches_official() {
        use crate::utils::permissions::denial_tracking::{DenialTrackingState, record_denial};

        let parent_state = DenialTrackingState {
            consecutive_denials: 1,
            total_denials: 1,
        };
        // CC permissions.ts:486-497 reads the mode from the child's live
        // getAppState, which forkedAgent.ts:362-374 derives from this parent.
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(ToolPermissionContext {
                    mode: PermissionMode::Auto,
                    ..Default::default()
                }),
                denial_tracking: Some(parent_state),
                ..Default::default()
            },
            None,
        );
        let parent = crate::tool::ToolUseContext::default().with_app_store(store.clone());

        let child = crate::utils::forked_agent::create_subagent_context(
            &parent,
            crate::utils::forked_agent::SubagentContextOverrides {
                share_set_app_state: false,
                ..Default::default()
            },
        );
        let local = child
            .local_denial_tracking
            .clone()
            .expect("CC forkedAgent.ts:422 gives an isolated fork its own state");
        local.set(DenialTrackingState {
            consecutive_denials: 3,
            total_denials: 3,
        });

        // Drive the PRODUCTION entry, sourcing every param the way
        // `services/tools/tool_execution.rs` does — that literal is where the
        // rail was severed, so a test that calls `persist_denial_tracking`
        // directly would pass with the defect restored. (It did; see
        // `validation.md` § 3 on revert probes proving only the path they drive.)
        let input = serde_json::json!({
            "todos": [{"content": "x", "status": "pending", "activeForm": "Doing x"}]
        });
        let decision = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu_isolated_denials",
                tool_name: "TodoWrite",
                mcp_info: None,
                input_summary: "1 item",
                input: &input,
                context: &child.tool_permission_context,
                messages: &[],
                app_store: child.app_store.store.as_ref(),
                local_denial_tracking: child.local_denial_tracking.clone(),
                abort_signal: None,
            },
            Some(&child),
        );

        assert!(matches!(
            decision,
            HasPermissionsToUseToolResult::Allow { .. }
        ));
        assert_eq!(
            local.get(),
            DenialTrackingState {
                consecutive_denials: 0,
                total_denials: 3,
            },
            "the child's OWN state is what recordSuccess advanced"
        );
        assert_eq!(
            store.get().denial_tracking,
            Some(parent_state),
            "the parent's store must be untouched; it is the same store object the \
             child inherited at forked_agent.rs:79, so a `None` local rail writes here"
        );
        let _ = record_denial;

        // Inverse: a SHARING fork has no local state of its own, so CC's else
        // branch writes to appState exactly as the parent's own turn would.
        let shared = crate::utils::forked_agent::create_subagent_context(
            &parent,
            crate::utils::forked_agent::SubagentContextOverrides {
                share_set_app_state: true,
                ..Default::default()
            },
        );
        assert!(
            shared.local_denial_tracking.is_none(),
            "CC shares the parent's handle, which is absent on a main-loop context"
        );
        persist_denial_tracking(
            shared.app_store.store.as_ref(),
            shared.local_denial_tracking.as_ref(),
            record_denial(parent_state),
        );
        assert_eq!(
            store.get().denial_tracking,
            Some(record_denial(parent_state)),
            "a sharing fork does write through to appState"
        );
    }

    #[test]
    fn auto_mode_allow_reset_matches_official_local_denial_tracking_precedence() {
        use crate::utils::permissions::denial_tracking::DenialTrackingState;

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _classifier = TranscriptClassifierGuard::on();

        let input = serde_json::json!({
            "todos": [{
                "content": "ship it",
                "status": "pending",
                "activeForm": "Shipping it"
            }]
        });
        let parent_state = DenialTrackingState {
            consecutive_denials: 3,
            total_denials: 9,
        };
        let store = store_with_denial_tracking(parent_state);
        let local = crate::tool::SharedDenialTracking::new(DenialTrackingState {
            consecutive_denials: 2,
            total_denials: 2,
        });
        let auto_context = ToolPermissionContext {
            mode: PermissionMode::Auto,
            ..ToolPermissionContext::default()
        };

        let decision = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu_todowrite_local_tracking",
            tool_name: "TodoWrite",
            mcp_info: None,
            input_summary: "1 item",
            input: &input,
            context: &auto_context,
            messages: &[],
            app_store: Some(&store),
            local_denial_tracking: Some(local.clone()),
            abort_signal: None,
        });

        assert!(matches!(
            decision,
            HasPermissionsToUseToolResult::Allow { .. }
        ));
        assert_eq!(
            local.get(),
            DenialTrackingState {
                consecutive_denials: 0,
                total_denials: 2,
            },
            "the local state is the one recordSuccess is applied to"
        );
        assert_eq!(
            store.get().denial_tracking,
            Some(parent_state),
            "persistDenialState must not reach the parent store when localDenialTracking is set"
        );
    }
    fn expected_allow(
        input: &serde_json::Value,
        reason: PermissionDecisionReason,
    ) -> HasPermissionsToUseToolResult {
        HasPermissionsToUseToolResult::Allow {
            updated_input: Some(input.clone()),
            user_modified: None,
            decision_reason: Some(reason),
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }

    #[test]
    fn bypass_and_plan_allow_after_tool_allow_match_official_complete_decision() {
        let input = serde_json::json!({"todos":[]});
        for mode in [PermissionMode::BypassPermissions, PermissionMode::Plan] {
            let permission = ToolPermissionContext {
                mode,
                is_bypass_permissions_mode_available: true,
                ..Default::default()
            };
            let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
                tool_use_id: "toolu-mode",
                tool_name: "TodoWrite",
                mcp_info: None,
                input_summary: "",
                input: &input,
                context: &permission,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            });
            assert_eq!(
                result,
                expected_allow(&input, PermissionDecisionReason::Mode { mode })
            );
        }
        let input = serde_json::json!({"value":1});
        let permission = ToolPermissionContext {
            mode: PermissionMode::Plan,
            is_bypass_permissions_mode_available: true,
            ..Default::default()
        };
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu-mcp",
            tool_name: "mcp__example__dynamic",
            mcp_info: None,
            input_summary: "",
            input: &input,
            context: &permission,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert_eq!(
            result,
            expected_allow(
                &input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::Plan
                }
            )
        );
    }

    #[test]
    fn aborted_permission_call_matches_official_rejection_before_rules_and_denial_reset() {
        let input = serde_json::json!({"todos":[]});
        let context = crate::tool::ToolUseContext::default();
        context.abort_controller.abort();
        let result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu-abort",
                tool_name: "TodoWrite",
                mcp_info: None,
                input_summary: "",
                input: &input,
                context: &context.tool_permission_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: Some(context.abort_controller.signal()),
            },
            Some(&context),
        );
        assert_eq!(
            result.into_decision(),
            Err(crate::utils::errors::AbortError::default())
        );
    }

    #[tokio::test]
    async fn rule_only_powershell_check_matches_official_content_deny_without_store_mutation() {
        let mut permissions = ToolPermissionContext {
            mode: PermissionMode::Default,
            ..Default::default()
        };
        permissions.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "PowerShell",
                Some("Write-Host:*".into()),
            )],
        );
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(permissions.clone()),
                ..Default::default()
            },
            None,
        );
        for writable in [false, true] {
            let mut context = crate::tool::ToolUseContext::default().with_app_store(store.clone());
            context.app_store.writable = writable;
            let decision = check_rule_based_permissions(
                &crate::tools::powershell_tool::powershell_tool_schema(),
                &serde_json::json!({"command":"Write-Host secret"}),
                &context,
            )
            .await;
            assert!(matches!(
                decision,
                Some(crate::types::permissions::PermissionDecision::Deny { .. })
            ));
            assert_eq!(*store.get().tool_permission_context, permissions);
        }
    }

    #[test]
    fn schema_failure_still_reaches_bypass_matches_official_inner_catch() {
        // CC permissions.ts:1218-1224 catches parse failures then proceeds to 2a.
        let input = serde_json::json!({"questions":42});
        let permission = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..Default::default()
        };
        let result = has_permissions_to_use_tool(HasPermissionsToUseToolParams {
            tool_use_id: "toolu-invalid",
            tool_name: "AskUserQuestion",
            mcp_info: None,
            input_summary: "",
            input: &input,
            context: &permission,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });
        assert_eq!(
            result,
            expected_allow(
                &input,
                PermissionDecisionReason::Mode {
                    mode: PermissionMode::BypassPermissions
                }
            )
        );
    }

    #[test]
    fn classifier_overflow_matches_official_manual_or_headless_rejection_before_denial_tracking() {
        use crate::utils::permissions::yolo_classifier::YoloClassifierDecision;
        for avoid_prompts in [false, true] {
            for unavailable in [false, true] {
                let input = serde_json::json!({"command":"custom-action"});
                let permission = ToolPermissionContext {
                    mode: PermissionMode::Auto,
                    should_avoid_permission_prompts: avoid_prompts,
                    ..Default::default()
                };
                let denials = crate::tool::SharedDenialTracking::new(Default::default());
                let request = mock_permission_request_with_input(
                    "perm",
                    "toolu",
                    "Bash",
                    "custom",
                    input.clone(),
                    PermissionMode::Auto,
                );
                let original_message = request.message.clone();
                let decision = if unavailable {
                    YoloClassifierDecision::Unavailable {
                        reason: "too long".into(),
                        model: "test".into(),
                        transcript_too_long: true,
                    }
                } else {
                    YoloClassifierDecision::Block {
                        reason: "too long".into(),
                        transcript_too_long: true,
                    }
                };
                let result = resolve_auto_mode_classifier_decision(
                    HasPermissionsToUseToolParams {
                        tool_use_id: "toolu",
                        tool_name: "Bash",
                        mcp_info: None,
                        input_summary: "",
                        input: &input,
                        context: &permission,
                        messages: &[],
                        app_store: None,
                        local_denial_tracking: Some(denials.clone()),
                        abort_signal: None,
                    },
                    request,
                    decision,
                );
                if avoid_prompts {
                    assert!(
                        matches!(result, HasPermissionsToUseToolResult::Aborted(ref error)
                        if error.message == "Agent aborted: auto mode classifier transcript exceeded context window in headless mode")
                    );
                } else {
                    let HasPermissionsToUseToolResult::Ask(request) = result else {
                        panic!("overflow must retain manual approval");
                    };
                    assert_eq!(request.message, original_message);
                    assert!(matches!(
                        request.decision_reason,
                        Some(PermissionDecisionReason::Other { .. })
                    ));
                }
                assert_eq!(denials.get(), Default::default());
            }
        }
    }

    #[test]
    fn live_deny_rule_precedes_stale_bypass_snapshot_matches_official_get_app_state() {
        let mut permission = ToolPermissionContext::default();
        permission.always_deny_rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("TodoWrite", None)],
        );
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState {
                tool_permission_context: std::sync::Arc::new(permission),
                ..Default::default()
            },
            None,
        );
        let mut context = crate::tool::ToolUseContext::default().with_app_store(store);
        context.tool_permission_context = ToolPermissionContext {
            mode: PermissionMode::BypassPermissions,
            ..Default::default()
        };
        let input = serde_json::json!({"todos":[]});
        let result = has_permissions_to_use_tool_with_context(
            HasPermissionsToUseToolParams {
                tool_use_id: "toolu-live",
                tool_name: "TodoWrite",
                mcp_info: None,
                input_summary: "",
                input: &input,
                context: &context.tool_permission_context,
                messages: &[],
                app_store: None,
                local_denial_tracking: None,
                abort_signal: None,
            },
            Some(&context),
        );
        assert!(matches!(result, HasPermissionsToUseToolResult::Deny(_)));
    }

    #[test]
    fn classifier_denial_limit_matches_official_headless_abort_preserves_incremented_state() {
        use crate::utils::permissions::denial_tracking::{DENIAL_LIMITS, DenialTrackingState};
        use crate::utils::permissions::yolo_classifier::YoloClassifierDecision;
        for hit_total in [false, true] {
            let input = serde_json::json!({"command":"blocked"});
            let permission = ToolPermissionContext {
                mode: PermissionMode::Auto,
                should_avoid_permission_prompts: true,
                ..Default::default()
            };
            let before = DenialTrackingState {
                consecutive_denials: if hit_total {
                    0
                } else {
                    DENIAL_LIMITS.max_consecutive - 1
                },
                total_denials: if hit_total {
                    DENIAL_LIMITS.max_total - 1
                } else {
                    0
                },
            };
            let tracking = crate::tool::SharedDenialTracking::new(before);
            let request = mock_permission_request_with_input(
                "perm",
                "toolu",
                "Bash",
                "blocked",
                input.clone(),
                PermissionMode::Auto,
            );
            let result = resolve_auto_mode_classifier_decision(
                HasPermissionsToUseToolParams {
                    tool_use_id: "toolu",
                    tool_name: "Bash",
                    mcp_info: None,
                    input_summary: "blocked",
                    input: &input,
                    context: &permission,
                    messages: &[],
                    app_store: None,
                    local_denial_tracking: Some(tracking.clone()),
                    abort_signal: None,
                },
                request,
                YoloClassifierDecision::Block {
                    reason: "blocked".into(),
                    transcript_too_long: false,
                },
            );
            assert!(
                matches!(result, HasPermissionsToUseToolResult::Aborted(ref error)
                if error.message == "Agent aborted: too many classifier denials in headless mode")
            );
            assert_eq!(tracking.get().total_denials, before.total_denials + 1);
            assert_eq!(
                tracking.get().consecutive_denials,
                before.consecutive_denials + 1
            );
        }
    }
}
