//! Incremental port of official `hooks/useCanUseTool.tsx`.
//! Official `useCanUseTool` receives a tool definition, parsed input,
//! tool-use execution context, assistant message, and toolUseID. This Rust
//! boundary keeps those source-level inputs: typed model `ToolUseBlock`s are
//! converted at the orchestration edge, then permission checks operate on a
//! `ToolUsePermissionRequest` instead of renderer message enums.

use crate::hooks::tool_permission::handlers::interactive_handler::handle_interactive_permission;
use crate::tool::ToolPermissionContext;
use crate::types::message::{AssistantMessage, ToolUseBlock};
use crate::types::message::{RenderableMessage, RenderableMessageKind, ToolUseStatus};
use crate::types::permissions::{PermissionBehavior, PermissionDecision, ToolUseConfirm};
use crate::types::tools::Tool;
pub use crate::utils::permissions::permissions::HasPermissionsToUseToolResult as CanUseToolResult;
use crate::utils::permissions::permissions::{
    HasPermissionsToUseToolParams, has_permissions_to_use_tool_with_context,
};

/// Nested-permission callback passed into tool execution.
/// Maps to: CC `hooks/useCanUseTool.tsx` `CanUseToolFn` (:44-53):
/// `(tool, input, toolUseContext, assistantMessage, toolUseID, forceDecision?)
/// => Promise<PermissionDecision>`. The carried type is the canonical
/// `PermissionDecision` union (#156); CC's fn is async, this borrow stays sync
/// because the call sites that hand it into `Tool::call` are still on the
/// synchronous orchestration leg (R3b debt) — only the decision type changed.
pub type CanUseToolFn<'a> = &'a (
        dyn Fn(
    &Tool,
    &serde_json::Value,
    &crate::tool::ToolUseContext,
    &AssistantMessage,
    &str,
    Option<PermissionDecision>,
) -> Result<PermissionDecision, crate::utils::errors::AbortError>
            + Send
            + Sync
    );

/// Owned Rust carrier for CC's async `CanUseToolFn`.
///
/// Maps to: CC `hooks/useCanUseTool.tsx:44-53` — `(tool, input, toolUseContext,
/// assistantMessage, toolUseID, forceDecision?) => Promise<PermissionDecision>`
/// — and `utils/forkedAgent.ts:489-558`. Declared HERE because CC declares the
/// fn type in `hooks/useCanUseTool.tsx` and `Tool.ts:12` imports it
/// (`import type { CanUseToolFn } from './hooks/useCanUseTool.js'`);
/// `src/tool.rs` mirrors that import with a `pub use` for its
/// `ToolUseContext.can_use_tool` field (#156 placement fix). The carried type
/// is the canonical [`PermissionDecision`] union (#156); the reduced Rust-only
/// `PromptDecision` no longer travels this pipe. CC's fn is async; this
/// carrier keeps a sync leg alongside the async one because parts of the Rust
/// orchestration are still synchronous (the R3b debt noted in
/// `services/tools/tool_execution.rs`) — only the decision type changed.
/// Existing synchronous policies use [`Self::new`]; consumers such as
/// speculation use [`Self::new_async`] so permission checks can perform
/// copy-on-write and return `updatedInput` before execution.
#[derive(Clone, Default)]
pub struct CanUseToolCallback(Option<std::sync::Arc<CanUseToolCallbackKind>>);

type SyncCanUseToolCallback = dyn Fn(
        &Tool,
        &serde_json::Value,
        &crate::tool::ToolUseContext,
        &AssistantMessage,
        &str,
        Option<PermissionDecision>,
    ) -> PermissionDecision
    + Send
    + Sync;

type AsyncCanUseToolCallback = dyn for<'a> Fn(
        &'a Tool,
        &'a serde_json::Value,
        &'a crate::tool::ToolUseContext,
        &'a AssistantMessage,
        &'a str,
        Option<PermissionDecision>,
    ) -> futures::future::BoxFuture<
        'a,
        Result<PermissionDecision, crate::utils::errors::AbortError>,
    > + Send
    + Sync;

enum CanUseToolCallbackKind {
    Sync(std::sync::Arc<SyncCanUseToolCallback>),
    Async(std::sync::Arc<AsyncCanUseToolCallback>),
}

impl CanUseToolCallback {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(
                &Tool,
                &serde_json::Value,
                &crate::tool::ToolUseContext,
                &AssistantMessage,
                &str,
                Option<PermissionDecision>,
            ) -> PermissionDecision
            + Send
            + Sync
            + 'static,
    {
        Self(Some(std::sync::Arc::new(CanUseToolCallbackKind::Sync(
            std::sync::Arc::new(callback),
        ))))
    }

    pub fn new_async<F>(callback: F) -> Self
    where
        F: for<'a> Fn(
                &'a Tool,
                &'a serde_json::Value,
                &'a crate::tool::ToolUseContext,
                &'a AssistantMessage,
                &'a str,
                Option<PermissionDecision>,
            ) -> futures::future::BoxFuture<'a, PermissionDecision>
            + Send
            + Sync
            + 'static,
    {
        Self::new_async_fallible(move |tool, input, context, assistant, id, forced| {
            let future = callback(tool, input, context, assistant, id, forced);
            Box::pin(async move { Ok(future.await) })
        })
    }

    /// L1 `Promise.reject(AbortError)` transport for the canonical permission owner.
    pub fn new_async_fallible<F>(callback: F) -> Self
    where
        F: for<'a> Fn(
                &'a Tool,
                &'a serde_json::Value,
                &'a crate::tool::ToolUseContext,
                &'a AssistantMessage,
                &'a str,
                Option<PermissionDecision>,
            ) -> futures::future::BoxFuture<
                'a,
                Result<PermissionDecision, crate::utils::errors::AbortError>,
            > + Send
            + Sync
            + 'static,
    {
        Self(Some(std::sync::Arc::new(CanUseToolCallbackKind::Async(
            std::sync::Arc::new(callback),
        ))))
    }

    pub fn decide(
        &self,
        tool: &Tool,
        input: &serde_json::Value,
        context: &crate::tool::ToolUseContext,
        assistant_message: &AssistantMessage,
        tool_use_id: &str,
        force_decision: Option<PermissionDecision>,
    ) -> Result<Option<PermissionDecision>, crate::utils::errors::AbortError> {
        let Some(callback) = self.0.as_deref() else {
            return Ok(None);
        };
        match callback {
            CanUseToolCallbackKind::Sync(callback) => Ok(Some(callback(
                tool,
                input,
                context,
                assistant_message,
                tool_use_id,
                force_decision,
            ))),
            // A4 exemption (PORTING.md § "Node-async → tokio"): sync bridges
            // normally go through `process_runtime::block_on_from_sync`, but
            // its future must be `Send + 'static` and this one borrows the
            // `'a` call arguments (Tool/context/message references). A scoped
            // thread with a scratch current_thread runtime is the same shape
            // as that bridge's current_thread arm — no `block_in_place`, so no
            // panic on a current_thread caller — just expressed for borrowed
            // futures. If the callback signatures ever become owned/'static,
            // collapse this onto `block_on_from_sync`.
            CanUseToolCallbackKind::Async(callback) => std::thread::scope(|scope| {
                let worker = std::thread::Builder::new()
                    .name("fork-can-use-tool".to_string())
                    .spawn_scoped(scope, || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|error| error.to_string())?;
                        Ok::<_, String>(runtime.block_on(callback(
                            tool,
                            input,
                            context,
                            assistant_message,
                            tool_use_id,
                            force_decision,
                        )))
                    });
                match worker.and_then(|worker| {
                    worker
                        .join()
                        .map_err(|_| std::io::Error::other("async canUseTool worker panicked"))
                }) {
                    Ok(Ok(decision)) => decision.map(Some),
                    Ok(Err(error)) => Ok(Some(failed_can_use_tool_decision(&error))),
                    Err(error) => Ok(Some(failed_can_use_tool_decision(&error.to_string()))),
                }
            }),
        }
    }

    pub fn decide_async<'a>(
        &'a self,
        tool: &'a Tool,
        input: &'a serde_json::Value,
        context: &'a crate::tool::ToolUseContext,
        assistant_message: &'a AssistantMessage,
        tool_use_id: &'a str,
        force_decision: Option<PermissionDecision>,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<PermissionDecision>, crate::utils::errors::AbortError>,
    > {
        Box::pin(async move {
            let Some(callback) = self.0.as_deref() else {
                return Ok(None);
            };
            match callback {
                CanUseToolCallbackKind::Sync(callback) => Ok(Some(callback(
                    tool,
                    input,
                    context,
                    assistant_message,
                    tool_use_id,
                    force_decision,
                ))),
                CanUseToolCallbackKind::Async(callback) => callback(
                    tool,
                    input,
                    context,
                    assistant_message,
                    tool_use_id,
                    force_decision,
                )
                .await
                .map(Some),
            }
        })
    }

    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

fn failed_can_use_tool_decision(error: &str) -> PermissionDecision {
    PermissionDecision::Deny {
        message: format!("canUseTool callback failed: {error}"),
        decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
            reason: "canUseTool callback failed".to_string(),
        },
        tool_use_id: None,
    }
}

impl std::fmt::Debug for CanUseToolCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "CanUseToolCallback(set)"
        } else {
            "CanUseToolCallback(unset)"
        })
    }
}

impl PartialEq for CanUseToolCallback {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolUsePermissionRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input_summary: String,
    /// Maps to official `ToolUseConfirm.input` / parsed model tool input.
    pub input: serde_json::Value,
}

impl ToolUsePermissionRequest {
    /// Maps to: CC `useCanUseTool.tsx` receiving the model `ToolUseBlock` and
    /// parsed input before permission evaluation.
    pub fn from_tool_use_block(block: &ToolUseBlock) -> Self {
        Self {
            tool_use_id: block.id.0.clone(),
            tool_name: block.name.clone(),
            input_summary: tool_use_input_summary(&block.name, &block.input),
            input: block.input.clone(),
        }
    }

    pub fn from_renderable_message(message: &RenderableMessage) -> Option<Self> {
        let RenderableMessageKind::Assistant { message: assistant } = &message.kind else {
            return None;
        };
        let Some(crate::types::message::AssistantContent::ToolUse(tool_use)) =
            assistant.first_content_block()
        else {
            return None;
        };

        let tool_use_id = if tool_use.id.0.is_empty() {
            message.uuid.clone()
        } else {
            tool_use.id.0.clone()
        };
        Some(Self {
            tool_use_id,
            tool_name: tool_use.name.clone(),
            input_summary: tool_use_input_summary(&tool_use.name, &tool_use.input),
            input: tool_use.input.clone(),
        })
    }
}

fn tool_use_input_summary(tool_name: &str, input: &serde_json::Value) -> String {
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
        _ => serde_json::to_string(input).unwrap_or_default(),
    }
}

/// Parameters for the official-shaped `canUseTool(...)` permission decision.
/// Maps to CC `hooks/useCanUseTool.tsx` `CanUseToolFn` inputs.
/// # Why five fields are private
///
/// CC's `canUseTool` takes the `ToolUseContext` itself and reads what it needs
/// off it at each use — `context.messages`, `context.getAppState()`,
/// `context.localDenialTracking`, `context.abortController.signal`. This port
/// flattens them onto a params struct, which means a caller holding a context
/// can hand-write them, and can therefore silently drop one.
///
/// That is not hypothetical. All four production sites in
/// `services/tools/tool_execution.rs` hardcoded `local_denial_tracking: None`,
/// so an isolated subagent's denials wrote into the PARENT's store (task #130).
/// Nothing failed, because a struct literal that omits nothing still compiles.
///
/// So every context-derived field is private to this module and reachable only
/// through [`CanUseToolParams::from_tool_use_context`]. A caller with a context
/// cannot drop one without a compile error. Callers with no `ToolUseContext` —
/// the UI-only helpers below, which have only a `ToolPermissionContext` — build
/// the struct directly, which is why the fields are module-private rather than
/// removed. Same shape as task #119's fix for query-path tool restrictions.
pub struct CanUseToolParams<'a> {
    pub tool: Option<&'a Tool>,
    pub tool_use: &'a ToolUsePermissionRequest,
    /// Exact invocation context passed by production orchestration. UI-only
    /// permission helpers may omit it and use the process-cwd fallback.
    pub(super) tool_use_context: Option<&'a crate::tool::ToolUseContext>,
    pub(super) context: &'a ToolPermissionContext,
    pub assistant_message: Option<&'a AssistantMessage>,
    pub tool_use_id: &'a str,
    /// Maps to CC `useCanUseTool.tsx` `forceDecision` — a nested caller can
    /// supply the already-computed decision and bypass permission checks.
    pub force_decision: Option<PermissionDecision>,
    /// Conversation history for auto-mode classifier.
    /// Maps to: CC `ToolUseContext.messages` → `classifyYoloAction(context.messages, ...)`.
    pub(super) messages: &'a [crate::types::message::Message],
    /// Optional AppStore for persisting `denialTracking` (CC `context.setAppState`).
    pub(super) app_store: Option<&'a crate::state::store::AppStore>,
    /// Maps to CC `ToolUseContext.localDenialTracking` for isolated subagents.
    /// The context's shared handle, mirroring CC's in-place `Object.assign`
    /// write (`permissions.ts:967-968`).
    pub(super) local_denial_tracking: Option<crate::tool::SharedDenialTracking>,
    /// Maps to: CC `ToolUseContext.abortController.signal` → classifyYoloAction / sideQuery.
    pub(super) abort_signal: Option<anthropic_sdk::AbortSignal>,
}

impl<'a> CanUseToolParams<'a> {
    /// The only way to build these params from a `ToolUseContext`.
    ///
    /// Maps to: CC `canUseTool(tool, input, toolUseContext, assistantMessage,
    /// toolUseID, forceDecision)` (`hooks/useCanUseTool.tsx:44`) — CC passes the
    /// context and lets the permission engine read it. Every field this fills is
    /// one CC reads off `context`; adding another context-derived field belongs
    /// here, not at a call site.
    pub fn from_tool_use_context(
        context: &'a crate::tool::ToolUseContext,
        tool: Option<&'a Tool>,
        tool_use: &'a ToolUsePermissionRequest,
        tool_use_id: &'a str,
        assistant_message: Option<&'a AssistantMessage>,
        force_decision: Option<PermissionDecision>,
    ) -> Self {
        Self {
            tool,
            tool_use,
            tool_use_context: Some(context),
            context: &context.tool_permission_context,
            assistant_message,
            tool_use_id,
            force_decision,
            messages: &context.messages,
            app_store: context.app_store.store.as_ref(),
            local_denial_tracking: context.local_denial_tracking.clone(),
            abort_signal: Some(context.abort_controller.signal()),
        }
    }
}

/// Maps to: CC `hooks/useCanUseTool.tsx` `canUseTool(...)`.
pub fn can_use_tool(params: CanUseToolParams<'_>) -> CanUseToolResult {
    let tool_name = params
        .tool
        .map(|tool| tool.name.as_str())
        .unwrap_or(params.tool_use.tool_name.as_str());
    let interactive_context = || {
        params.tool_use_context.filter(|context| {
            !context
                .get_app_state()
                .map(|state| {
                    state
                        .tool_permission_context
                        .should_avoid_permission_prompts
                })
                .unwrap_or(
                    context
                        .tool_permission_context
                        .should_avoid_permission_prompts,
                )
        })
    };
    // Maps to: CC useCanUseTool.tsx:81,330-346 and PermissionContext.ts:148-176.
    // This UI boundary resolves cancellation to Ask; canonical/headless callbacks
    // still transport the rejected AbortError to their own caller.
    let cancel = |context: &crate::tool::ToolUseContext| {
        let message = crate::hooks::tool_permission::permission_context::cancel_and_abort(
            tool_name,
            context,
            None,
            true,
            &[],
        );
        forced_decision_to_can_use_tool_result(
            PermissionDecision::Ask {
                message,
                updated_input: None,
                decision_reason: None,
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            },
            tool_name,
            params.tool_use,
            params.context,
        )
    };
    if let Some(context) =
        interactive_context().filter(|context| context.abort_controller.is_aborted())
    {
        return cancel(context);
    }
    let result = if let Some(decision) = params.force_decision {
        forced_decision_to_can_use_tool_result(decision, tool_name, params.tool_use, params.context)
    } else if let Some(tool) = params.tool {
        let mut canonical_tool_use = params.tool_use.clone();
        canonical_tool_use.tool_name = tool.name.clone();
        check_can_use_tool(
            &canonical_tool_use,
            tool.mcp_info.as_ref(),
            params.context,
            params.tool_use_context,
            params.messages,
            params.app_store,
            params.local_denial_tracking,
            params.abort_signal,
        )
    } else {
        check_can_use_tool(
            params.tool_use,
            None,
            params.context,
            params.tool_use_context,
            params.messages,
            params.app_store,
            params.local_denial_tracking,
            params.abort_signal,
        )
    };
    if let (CanUseToolResult::Aborted(_), Some(context)) = (&result, interactive_context()) {
        return cancel(context);
    }
    // Maps to: CC useCanUseTool.tsx:138-185, deny / auto-mode classifier.
    // This is the interactive canUseTool owner, not the lower permission
    // policy: forced decisions pass through the same recording branch.
    if crate::utils::permissions::permission_setup::is_transcript_classifier_feature_enabled() {
        if let CanUseToolResult::Deny(request) = &result {
            if let Some(crate::types::permissions::PermissionDecisionReason::Classifier {
                classifier,
                reason,
            }) = &request.decision_reason
            {
                if classifier == "auto-mode" {
                    let description_and_name = if let Some(mcp_info) =
                        params.tool.and_then(|tool| tool.mcp_info.as_ref())
                    {
                        params.tool_use_context.and_then(|context| {
                            context
                                .mcp_state
                                .clients
                                .iter()
                                .find(|server| server.client.name == mcp_info.server_name)
                                .and_then(|server| {
                                    server
                                        .tools
                                        .iter()
                                        .find(|tool| tool.name == mcp_info.tool_name)
                                })
                                .map(|tool| {
                                    let display_name =
                                        crate::services::mcp::client::mcp_tool_display_name(
                                            tool.display_name.as_deref(),
                                            &tool.name,
                                        );
                                    (
                                        crate::services::mcp::client::mcp_tool_description(tool),
                                        crate::services::mcp::client::mcp_tool_user_facing_name(
                                            &mcp_info.server_name,
                                            &display_name,
                                        ),
                                    )
                                })
                        })
                    } else {
                        crate::services::tools::tool_execution::find_tool_call(tool_name).map(
                            |tool| {
                                (
                                    tool.description(&params.tool_use.input),
                                    tool.user_facing_name(Some(&params.tool_use.input)),
                                )
                            },
                        )
                    };
                    // Source rechecks abort after awaiting description before
                    // recording or notifying. The current synchronous ToolCall
                    // description carrier resolves at this same boundary.
                    if let Some(context) = interactive_context()
                        .filter(|context| context.abort_controller.is_aborted())
                    {
                        return cancel(context);
                    }
                    // An unregistered custom tool or missing MCP snapshot has
                    // no description callback in this port. Do not substitute
                    // the model prompt or input summary for that missing owner.
                    if let Some((description, user_facing_name)) = description_and_name {
                        crate::utils::auto_mode_denials::record_auto_mode_denial(
                            crate::utils::auto_mode_denials::AutoModeDenial {
                                tool_name: tool_name.to_string(),
                                display: description,
                                reason: reason.clone(),
                                timestamp: chrono::Utc::now().timestamp_millis() as f64,
                            },
                        );
                        if let Some(callback) = params
                            .tool_use_context
                            .and_then(|context| context.add_notification.0.as_ref())
                        {
                            use crate::context::notifications::{
                                Notification, NotificationColor, NotificationPriority,
                                NotificationSegment,
                            };
                            let denied =
                                format!("{} denied by auto mode", user_facing_name.to_lowercase());
                            callback(
                                Notification::text(
                                    "auto-mode-denied",
                                    format!("{denied} · /permissions"),
                                    NotificationPriority::Immediate,
                                )
                                .with_segments(vec![
                                    NotificationSegment::text(denied)
                                        .with_color(NotificationColor::Error),
                                    NotificationSegment::text(" · /permissions").with_dim(true),
                                ]),
                            );
                        }
                    }
                }
            }
        }
    }
    result
}

/// Async `canUseTool` — Maps to CC `await canUseTool(...)` / `await hasPermissionsToUseTool`.
/// Prefer this from async contexts so classifier does not block the runtime.
pub async fn can_use_tool_async(params: CanUseToolParams<'_>) -> CanUseToolResult {
    let tool_name = params
        .tool
        .map(|tool| tool.name.as_str())
        .unwrap_or(params.tool_use.tool_name.as_str());
    if let Some(decision) = params.force_decision {
        return forced_decision_to_can_use_tool_result(
            decision,
            tool_name,
            params.tool_use,
            params.context,
        );
    }
    let tool_use = if let Some(tool) = params.tool {
        let mut canonical = params.tool_use.clone();
        canonical.tool_name = tool.name.clone();
        canonical
    } else {
        params.tool_use.clone()
    };
    crate::utils::permissions::permissions::has_permissions_to_use_tool_async_with_context(
        HasPermissionsToUseToolParams {
            tool_use_id: &tool_use.tool_use_id,
            tool_name: &tool_use.tool_name,
            mcp_info: params.tool.and_then(|tool| tool.mcp_info.as_ref()),
            input_summary: &tool_use.input_summary,
            input: &tool_use.input,
            context: params.context,
            messages: params.messages,
            app_store: params.app_store,
            local_denial_tracking: params.local_denial_tracking,
            abort_signal: params.abort_signal,
        },
        params.tool_use_context,
    )
    .await
}

/// Maps to: CC `useCanUseTool.tsx:83-92` — `forceDecision !== undefined ?
/// Promise.resolve(forceDecision) : hasPermissionsToUseTool(...)`. The forced
/// decision becomes the `result` the branches below consume; nothing about it
/// is re-derived. #156: the full canonical union now crosses this boundary, so
/// a Deny keeps its `message` / `decisionReason` (read downstream by
/// `toolExecution.ts:1023` and `PermissionRuleExplanation`) instead of being
/// flattened to defaults.
///
/// The three behaviors are NOT symmetric in CC, and this projection mirrors
/// that asymmetry:
/// - deny → `resolve(result)` (`:185`), the decision verbatim;
/// - ask  → handed to the dialog/handler chain (`:189-327`);
/// - allow → **rebuilt**, never forwarded (`:113-134` → `buildAllow`).
///
/// #185 ruling: the allow arm owns the strip. See its own comment for the
/// `buildAllow` field-by-field derivation and the producer census.
fn forced_decision_to_can_use_tool_result(
    decision: PermissionDecision,
    tool_name: &str,
    tool_use: &ToolUsePermissionRequest,
    context: &ToolPermissionContext,
) -> CanUseToolResult {
    let original_decision = decision.clone();
    match decision {
        // #185: CC does NOT forward a forced allow — it REBUILDS it, and this
        // is the layer that does the rebuilding, so the strip is owned HERE.
        //
        // `useCanUseTool.tsx:83-92` makes `Promise.resolve(forceDecision)` the
        // very `result` the allow branch at `:113-134` consumes, and that
        // branch resolves through
        //   `ctx.buildAllow(result.updatedInput ?? input,
        //                   { decisionReason: result.decisionReason })`
        // — `opts` carries `decisionReason` and nothing else. `buildAllow`
        // (`hooks/toolPermission/PermissionContext.ts:264-284`) returns exactly
        //   `{ behavior, updatedInput, userModified: opts?.userModified ?? false,
        //      ...(opts?.decisionReason && { decisionReason }),
        //      ...(opts?.acceptFeedback && { acceptFeedback }),
        //      ...(opts?.contentBlocks?.length && { contentBlocks }) }`
        // so `acceptFeedback`/`contentBlocks` appear only when the CALLER
        // supplies them — the dialog path `handleUserAllow` (`:291-317`) does,
        // this path does not — and `toolUseID` is not a member of `buildAllow`'s
        // output at all. CC therefore drops all three HERE and pins
        // `userModified` to `false` regardless of what the forced decision held.
        //
        // #170 had widened this projection to all six fields on the reading
        // that "CC returns the forced decision AS the result". That reading is
        // right for the DENY arm (`:185 resolve(result)`); it does not hold for
        // the allow arm, which is the one `buildAllow` rebuilds. Narrowing the
        // PROJECTION does not narrow the TYPE: `CanUseToolResult` stays six-wide
        // because it doubles as `hasPermissionsToUseTool`'s return — CC declares
        // that as a `CanUseToolFn` (`permissions.ts:473`), i.e. the full
        // `PermissionAllowDecision` — and the tool-result pass-through at
        // `utils/permissions/permissions.rs:650-667` still needs every field.
        // Only this one `canUseTool`-role rebuild is `buildAllow`-shaped.
        //
        // Producer census (ast-grep `PermissionDecision::Allow { $$$F }` over
        // `src/`, 5 hits): no production producer can reach this projection with
        // any of the three set. `services/prompt_suggestion/speculation.rs:71`
        // and `services/tools/tool_execution.rs:381` build them empty;
        // `types/permissions.rs:992` (`TryFrom<PermissionResult>`) has no
        // production caller; the SDK normalizer
        // (`utils/permissions/permission_prompt_tool_result_schema.rs:249`) is
        // the only site that sets `tool_use_id`, and its single consumer
        // terminates it in `PermissionPromptResponse::with_tool_use_id`
        // (`cli/structured_io.rs:782-793`) rather than feeding a
        // `force_decision`; `accept_feedback: Some(..)` occurs only in tests.
        // The strip is thus observationally free today, and it is what turns the
        // `..` in the three `tool_execution.rs` consumers (`:646`, `:734`,
        // `:1173`) from LOAD-BEARING into merely defensive.
        //
        // `updated_input` keeps CC's `result.updatedInput ?? input` as an
        // `Option`: `None` means "no override" and every consumer's fallback
        // branch is the original input verbatim. Materializing the `??` here
        // would NOT be equivalent — `permission_request_with_updated_input`
        // (`tool_execution.rs:1319-1330`) also rewrites `input_summary` and
        // `rule`, which CC's rebuild never touches.
        //
        // The dropped fields are named (not swept by a `..`) so a seventh field
        // on `PermissionAllowDecision` becomes a compile error here.
        PermissionDecision::Allow {
            updated_input,
            // `buildAllow`'s `opts?.userModified ?? false`: this call site
            // passes no `userModified`, so CC pins false and discards the
            // forced decision's own value.
            user_modified: _,
            decision_reason,
            tool_use_id: _,
            accept_feedback: _,
            content_blocks: _,
        } => CanUseToolResult::Allow {
            updated_input,
            user_modified: Some(false),
            decision_reason,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        },
        PermissionDecision::Deny {
            message,
            decision_reason,
            // CC keeps `toolUseID` on the deny object; no CC site reads it as
            // a property after normalization (ast-grep `$X.toolUseID` over
            // `../rebuild/src`: every hit is a ToolUseConfirm/progress/ctx
            // read), and this projection's target already carries the id as
            // `request.tool_use_id`.
            tool_use_id: _,
        } => {
            let mut request = forced_permission_request(tool_name, tool_use, context);
            request.permission_result = Some(original_decision);
            request.message = message;
            request.decision_reason = Some(decision_reason);
            CanUseToolResult::Deny(request)
        }
        // CC ask fields (`types/permissions.ts:199-226`) and where each lands:
        // message/updatedInput/decisionReason/suggestions/blockedPath/metadata
        // are projected below. The three the `..` still covers, each verified
        // against CC's readers:
        // - `isBashSecurityCheckForMisparsing`: every CC reader lives inside
        //   the Bash permission engine (`BashTool/bashSecurity.ts`,
        //   `BashTool/bashPermissions.ts:2083-2106`); nothing downstream of
        //   the `CanUseToolFn` boundary reads it.
        // - `pendingClassifierCheck`: CC reads it in `useCanUseTool.tsx`'s ask
        //   branch (`:199-234`, `:242-303`, BASH_CLASSIFIER); the port's
        //   coordinator/swarm handlers take a stubbed bool, so the carrier for
        //   the live value is the unported speculative-classifier wiring, not
        //   this projection (seam, booked with K-batch notes).
        // - `contentBlocks`: produced only by dialog rejects
        //   (`PermissionContext.ts:172`) as a canUseTool RESOLUTION and read
        //   at `toolExecution.ts:1040-1046`; the port's reject resolution
        //   travels on `PermissionPromptResponse.content_blocks`, never
        //   through this ask-to-dialog projection.
        PermissionDecision::Ask {
            message,
            updated_input,
            decision_reason,
            suggestions,
            blocked_path,
            metadata,
            ..
        } => {
            let mut request = forced_permission_request(tool_name, tool_use, context);
            // CC hands the ask's `updatedInput` to the dialog through
            // `toolUseConfirm.permissionResult`; this port's `PermissionRequest`
            // carries the input the dialog shows and executes on allow.
            if let Some(updated_input) = updated_input {
                request.input = updated_input;
            }
            request.permission_result = Some(original_decision);
            request.message = message;
            // `is_compound_command` is the port's cached projection of CC's
            // per-render `permissionResult.decisionReason?.type ===
            // 'subcommandResults'` (`BashPermissionRequest.tsx:212`); every
            // site that installs a decision reason must keep it in sync.
            request.is_compound_command = matches!(
                decision_reason,
                Some(crate::types::permissions::PermissionDecisionReason::SubcommandResults { .. })
            );
            request.decision_reason = decision_reason;
            request.suggestions = suggestions;
            request.blocked_path = blocked_path;
            request.metadata = metadata;
            CanUseToolResult::Ask(request)
        }
    }
}

fn forced_permission_request(
    tool_name: &str,
    tool_use: &ToolUsePermissionRequest,
    context: &ToolPermissionContext,
) -> crate::types::permissions::PermissionRequest {
    crate::utils::permissions::permissions::mock_permission_request_with_input(
        format!("perm-{}", tool_use.tool_use_id),
        tool_use.tool_use_id.clone(),
        tool_name.to_string(),
        tool_use.input_summary.clone(),
        tool_use.input.clone(),
        context.mode,
    )
}

pub fn check_can_use_tool(
    tool_use: &ToolUsePermissionRequest,
    mcp_info: Option<&crate::types::tools::McpToolInfo>,
    context: &ToolPermissionContext,
    tool_use_context: Option<&crate::tool::ToolUseContext>,
    messages: &[crate::types::message::Message],
    app_store: Option<&crate::state::store::AppStore>,
    local_denial_tracking: Option<crate::tool::SharedDenialTracking>,
    abort_signal: Option<anthropic_sdk::AbortSignal>,
) -> CanUseToolResult {
    has_permissions_to_use_tool_with_context(
        HasPermissionsToUseToolParams {
            tool_use_id: &tool_use.tool_use_id,
            tool_name: &tool_use.tool_name,
            mcp_info,
            input_summary: &tool_use.input_summary,
            input: &tool_use.input,
            context,
            // Maps to: CC `classifyYoloAction(context.messages, ...)`.
            messages,
            app_store,
            local_denial_tracking,
            // Maps to: CC `context.abortController.signal` → classifyYoloAction / sideQuery.
            abort_signal,
        },
        tool_use_context,
    )
}

pub fn can_use_tool_or_queue_permission(
    tool_use: &ToolUsePermissionRequest,
    context: &ToolPermissionContext,
    queue: &mut Vec<ToolUseConfirm>,
) -> CanUseToolResult {
    can_use_tool_or_queue_permission_for_params(
        CanUseToolParams {
            tool: None,
            tool_use,
            context,
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: None,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        },
        queue,
    )
}

pub fn can_use_tool_or_queue_permission_for_params(
    params: CanUseToolParams<'_>,
    queue: &mut Vec<ToolUseConfirm>,
) -> CanUseToolResult {
    can_use_tool_or_queue_permission_for_params_with_store(params, queue, None)
}

/// Like [`can_use_tool_or_queue_permission_for_params`], but when running as a
/// swarm worker forwards Ask to the leader (CC `handleSwarmWorkerPermission`)
/// and writes `AppState.pendingWorkerRequest` for the waiting indicator.
pub fn can_use_tool_or_queue_permission_for_params_with_store(
    params: CanUseToolParams<'_>,
    queue: &mut Vec<ToolUseConfirm>,
    app_store: Option<&crate::state::store::AppStore>,
) -> CanUseToolResult {
    let abort_controller = params
        .tool_use_context
        .map(|context| context.abort_controller.clone());
    let result = can_use_tool(params);
    // An already-resolved cancellation never enters interactiveHandler/queue.
    if abort_controller.is_some_and(|controller| controller.is_aborted()) {
        return result;
    }
    if let CanUseToolResult::Ask(mut request) = result.clone() {
        crate::hooks::tool_permission::handlers::interactive_handler::fill_tool_description(
            &mut request,
        );
        let wait_timeout = crate::utils::process_env::var("COMETIX_SWARM_PERMISSION_WAIT_MS")
            .and_then(|value| value.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
            .unwrap_or_else(|| std::time::Duration::from_millis(30_000));
        if let Some(decision) =
            crate::hooks::tool_permission::handlers::swarm_worker_handler::try_resolve_swarm_worker_ask(
                &request.tool_name,
                &request.tool_use_id,
                &request.description,
                &request.input,
                &[],
                app_store,
                wait_timeout,
                || false,
            )
        {
            return match decision.behavior {
                PermissionBehavior::Allow => CanUseToolResult::Allow {
                    updated_input: decision.updated_input,
                    user_modified: None,
                    decision_reason: None,
                    tool_use_id: None,
                    // CC's leader answer can carry `acceptFeedback`
                    // (`inProcessRunner.ts:287`); the port's swarm relay
                    // transport (`PromptDecision`) has no such field yet, so
                    // there is nothing to project here — swarm-relay payload
                    // parity is its own seam, not this projection's drop.
                    accept_feedback: None,
                    content_blocks: Vec::new(),
                },
                PermissionBehavior::Deny | PermissionBehavior::Ask => {
                    CanUseToolResult::Deny(request)
                }
            };
        }
        // CC interactiveHandler.ts:84: only the interactive handler uses
        // displayInput. The swarm request above and inProcess builder retain
        // ctx.input; description was already computed from that original input.
        if !crate::utils::teammate_context::get_teammate_context()
            .is_some_and(|identity| identity.is_in_process)
        {
            if let Some(PermissionDecision::Ask {
                updated_input: Some(input),
                ..
            }) = &request.permission_result
            {
                request.input = input.clone();
            }
        }
        handle_interactive_permission(queue, request.clone());
        return CanUseToolResult::Ask(request);
    }
    result
}

#[cfg(test)]
mod tests {
    /// The private fields make it a compile error for a cross-module caller to
    /// hand-write the context-derived params — that guarantee is the primary
    /// evidence for task #137 and cannot be expressed as a runtime assertion.
    /// Verified by probe: inserting a literal in `services/tools/tool_execution.rs`
    /// yields `E0451: fields context, tool_use_context, messages, app_store,
    /// local_denial_tracking and abort_signal of struct CanUseToolParams are private`.
    ///
    /// This test covers the other half — that the constructor reads each field
    /// from the RIGHT place on the context. A guard against omission does not
    /// catch a wrong source.
    #[test]
    fn from_tool_use_context_sources_every_field_from_the_context() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let denials = crate::tool::SharedDenialTracking::new(
            crate::utils::permissions::denial_tracking::DenialTrackingState {
                consecutive_denials: 4,
                total_denials: 7,
            },
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store.store = Some(store);
        context.local_denial_tracking = Some(denials.clone());
        context.messages = vec![crate::types::message::Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: "ctx-message".to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "hi".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            },
        )];
        context.tool_permission_context.mode =
            crate::types::permissions::PermissionMode::AcceptEdits;

        let tool_use = queued_bash();
        let params = CanUseToolParams::from_tool_use_context(
            &context,
            None,
            &tool_use,
            "toolu_probe",
            None,
            None,
        );

        // CC `permissions.ts:490`/`:556` — the field whose absence was #130.
        assert_eq!(
            params
                .local_denial_tracking
                .as_ref()
                .map(crate::tool::SharedDenialTracking::get),
            Some(denials.get()),
            "the denial rail must come from the context, not be defaulted"
        );
        assert_eq!(
            params.context.mode,
            crate::types::permissions::PermissionMode::AcceptEdits,
            "the permission context is the context's own, not a default"
        );
        assert_eq!(
            params.messages.len(),
            1,
            "CC classifyYoloAction(context.messages, ...)"
        );
        assert!(params.app_store.is_some());
        assert!(params.tool_use_context.is_some());
        assert!(
            params.abort_signal.is_some(),
            "CC context.abortController.signal reaches classifyYoloAction / sideQuery"
        );
    }

    use super::*;
    use crate::types::permissions::{
        PermissionDecisionReason, PermissionMode, PermissionRequest, PermissionRuleSource,
        PermissionRuleValue,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::collections::HashMap;

    fn queued_bash() -> ToolUsePermissionRequest {
        ToolUsePermissionRequest {
            tool_use_id: "toolu_mock".to_string(),
            tool_name: "Bash".to_string(),
            input_summary: "cargo test --quiet".to_string(),
            input: serde_json::json!({ "command": "cargo test --quiet" }),
        }
    }

    #[test]
    fn can_use_tool_accepts_official_shaped_params() {
        let tool_use = queued_bash();
        let assistant_message = AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                ToolUseBlock {
                    id: crate::types::ids::ToolUseId(tool_use.tool_use_id.clone()),
                    name: tool_use.tool_name.clone(),
                    input: tool_use.input.clone(),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        };

        let result = can_use_tool(CanUseToolParams {
            tool: None,
            tool_use: &tool_use,
            context: &ToolPermissionContext::default(),
            tool_use_context: None,
            assistant_message: Some(&assistant_message),
            tool_use_id: &tool_use.tool_use_id,
            force_decision: None,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        assert!(matches!(result, CanUseToolResult::Ask(_)));
    }

    #[test]
    fn can_use_tool_uses_tool_definition_canonical_name() {
        let mut tool_use = queued_bash();
        tool_use.tool_name = "LegacyBashAlias".to_string();
        let bash_tool = crate::tools::bash_tool::bash_tool_schema();

        let result = can_use_tool(CanUseToolParams {
            tool: Some(&bash_tool),
            tool_use: &tool_use,
            context: &ToolPermissionContext::default(),
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: None,
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        assert!(matches!(
            result,
            CanUseToolResult::Ask(PermissionRequest { rule, .. }) if rule.tool_name == "Bash"
        ));
    }

    #[test]
    fn can_use_tool_honors_force_decision_without_rule_lookup() {
        let tool_use = queued_bash();
        let mut context = ToolPermissionContext::default();
        context.mode = PermissionMode::BypassPermissions;

        let result = can_use_tool(CanUseToolParams {
            tool: None,
            tool_use: &tool_use,
            context: &context,
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: Some(PermissionDecision::Deny {
                message: "forced deny".to_string(),
                decision_reason: PermissionDecisionReason::Other {
                    reason: "forced".to_string(),
                },
                tool_use_id: None,
            }),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        assert!(matches!(
            result,
            CanUseToolResult::Deny(PermissionRequest { tool_use_id, .. }) if tool_use_id == "toolu_mock"
        ));
    }

    /// #156: the forced Deny keeps its `message`/`decisionReason` on the way
    /// to the request — CC `useCanUseTool.tsx:185` resolves the decision
    /// verbatim, and `toolExecution.ts:1023` reads `permissionDecision.message`
    /// as the model-facing error. The pre-#156 `PromptDecision` pipe dropped
    /// both (the request came back with an empty `message` and no
    /// `decision_reason`).
    #[test]
    fn forced_deny_decision_keeps_message_and_decision_reason() {
        let tool_use = queued_bash();
        let result = can_use_tool(CanUseToolParams {
            tool: None,
            tool_use: &tool_use,
            context: &ToolPermissionContext::default(),
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: Some(PermissionDecision::Deny {
                message: "Tool use is not allowed during compaction".to_string(),
                decision_reason: PermissionDecisionReason::Other {
                    reason: "compaction agent should only produce text summary".to_string(),
                },
                tool_use_id: None,
            }),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let CanUseToolResult::Deny(request) = result else {
            panic!("forced deny must stay a deny");
        };
        assert_eq!(request.message, "Tool use is not allowed during compaction");
        assert_eq!(
            request.decision_reason,
            Some(PermissionDecisionReason::Other {
                reason: "compaction agent should only produce text summary".to_string(),
            })
        );
    }

    /// The maximal forced allow — every field of CC's `PermissionAllowDecision`
    /// (`types/permissions.ts:174-184`) set — used by both #185 shape tests.
    fn maximal_forced_allow() -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: Some(serde_json::json!({"command": "echo rewritten"})),
            user_modified: Some(true),
            decision_reason: Some(PermissionDecisionReason::Other {
                reason: "speculation_file_access".to_string(),
            }),
            tool_use_id: Some("toolu_forced".to_string()),
            accept_feedback: Some("looks safe".to_string()),
            content_blocks: vec![serde_json::json!({"type": "text", "text": "extra"})],
        }
    }

    /// #185 layer 1 — the projection function itself.
    ///
    /// Maps to: CC `hooks/useCanUseTool.tsx:83-92` + `:113-134`. A forced
    /// decision becomes `result`, and an allow `result` is REBUILT through
    /// `ctx.buildAllow(result.updatedInput ?? input, { decisionReason })`, whose
    /// output (`hooks/toolPermission/PermissionContext.ts:264-284`) is exactly
    /// `{ behavior, updatedInput, userModified: false, decisionReason? }`.
    /// `toolUseID` / `acceptFeedback` / `contentBlocks` are not in it.
    ///
    /// Fails on the #156 shape (`user_modified` forwarded as `Some(true)`) and
    /// on the #170 shape (all six forwarded), which is the ownership question
    /// this pins: the strip lives here, not in the consumers' `..`.
    #[test]
    fn forced_allow_decision_projects_official_build_allow_shape() {
        let tool_use = queued_bash();
        let result = forced_decision_to_can_use_tool_result(
            maximal_forced_allow(),
            "Bash",
            &tool_use,
            &ToolPermissionContext::default(),
        );

        assert_eq!(
            result,
            CanUseToolResult::Allow {
                // `result.updatedInput ?? input` — the decision carries one.
                updated_input: Some(serde_json::json!({"command": "echo rewritten"})),
                // `opts?.userModified ?? false`; the decision's `Some(true)`
                // is discarded because this call site passes no `userModified`.
                user_modified: Some(false),
                // The one field `buildAllow` spreads for this caller.
                decision_reason: Some(PermissionDecisionReason::Other {
                    reason: "speculation_file_access".to_string(),
                }),
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            }
        );
    }

    /// #185 layer 2 — the `canUseTool` boundary a consumer actually calls.
    ///
    /// Same contract one call up: `can_use_tool` must not hand a consumer more
    /// than `buildAllow` emits, so the `..` in the three
    /// `services/tools/tool_execution.rs` consumers (`:646`, `:734`, `:1173`)
    /// is defensive rather than load-bearing. The consumer-side half is pinned
    /// by `tool_execution.rs`'s
    /// `required_can_use_tool_allow_projects_official_build_allow_shape`.
    #[test]
    fn can_use_tool_forced_allow_reaches_consumers_build_allow_shaped() {
        let tool_use = queued_bash();
        let result = can_use_tool(CanUseToolParams {
            tool: None,
            tool_use: &tool_use,
            context: &ToolPermissionContext::default(),
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: Some(maximal_forced_allow()),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let CanUseToolResult::Allow {
            updated_input,
            user_modified,
            decision_reason,
            tool_use_id,
            accept_feedback,
            content_blocks,
        } = result
        else {
            panic!("forced allow must stay an allow");
        };
        assert_eq!(
            updated_input,
            Some(serde_json::json!({"command": "echo rewritten"}))
        );
        assert_eq!(user_modified, Some(false));
        assert!(decision_reason.is_some());
        assert_eq!(
            (tool_use_id, accept_feedback, content_blocks),
            (None, None, Vec::new()),
            "CC's buildAllow output has no toolUseID/acceptFeedback/contentBlocks \
             for this caller (PermissionContext.ts:264-284)"
        );
    }

    /// #170: a forced Ask that carries a `subcommandResults` decision reason
    /// must keep the port's cached `is_compound_command` projection in sync —
    /// CC recomputes `permissionResult.decisionReason?.type ===
    /// 'subcommandResults'` per render (`BashPermissionRequest.tsx:212`).
    /// Fails on the old shape: the forced-ask projection installed the reason
    /// but left `is_compound_command` at its `false` default.
    #[test]
    fn forced_ask_with_subcommand_results_marks_compound_command() {
        let tool_use = queued_bash();
        let result = can_use_tool(CanUseToolParams {
            tool: None,
            tool_use: &tool_use,
            context: &ToolPermissionContext::default(),
            tool_use_context: None,
            assistant_message: None,
            tool_use_id: &tool_use.tool_use_id,
            force_decision: Some(PermissionDecision::Ask {
                message: "compound".to_string(),
                updated_input: None,
                decision_reason: Some(PermissionDecisionReason::SubcommandResults {
                    reasons: std::collections::BTreeMap::new(),
                }),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            }),
            messages: &[],
            app_store: None,
            local_denial_tracking: None,
            abort_signal: None,
        });

        let CanUseToolResult::Ask(request) = result else {
            panic!("forced ask must stay an ask");
        };
        assert!(request.is_compound_command);
        assert!(matches!(
            request.decision_reason,
            Some(PermissionDecisionReason::SubcommandResults { .. })
        ));
    }

    #[test]
    fn queued_tool_use_asks_and_pushes_queue() {
        let mut queue = Vec::new();
        let result = can_use_tool_or_queue_permission(
            &queued_bash(),
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].tool_use_id(), "toolu_mock");
    }

    #[test]
    fn read_tools_in_working_directory_allow_without_queueing_prompt() {
        let mut queue = Vec::new();
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_read".to_string(),
            tool_name: "Read".to_string(),
            input_summary: "Cargo.toml".to_string(),
            input: serde_json::json!({"file_path": "Cargo.toml"}),
        };

        let result = can_use_tool_or_queue_permission(
            &tool_use,
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn config_tool_get_allows_but_set_requests_permission() {
        let mut queue = Vec::new();
        let get_config = ToolUsePermissionRequest {
            tool_use_id: "toolu_config_get".to_string(),
            tool_name: "Config".to_string(),
            input_summary: "theme".to_string(),
            input: serde_json::json!({"setting": "theme"}),
        };
        let result = can_use_tool_or_queue_permission(
            &get_config,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let set_config = ToolUsePermissionRequest {
            tool_use_id: "toolu_config_set".to_string(),
            tool_name: "Config".to_string(),
            input_summary: "theme = dark".to_string(),
            input: serde_json::json!({"setting": "theme", "value": "dark"}),
        };
        let result = can_use_tool_or_queue_permission(
            &set_config,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn lsp_tool_uses_read_permission_rules_for_file_path() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _lsp = EnvVarGuard::set("ENABLE_LSP_TOOL", "1");
        let mut queue = Vec::new();
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_lsp".to_string(),
            tool_name: "LSP".to_string(),
            input_summary: "src/main.rs".to_string(),
            input: serde_json::json!({
                "operation": "findReferences",
                "filePath": "src/main.rs",
                "line": 1,
                "character": 1
            }),
        };

        let result = can_use_tool_or_queue_permission(
            &tool_use,
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn read_tools_outside_working_directory_still_prompt() {
        let mut queue = Vec::new();
        let outside =
            std::env::temp_dir().join(format!("cometix-outside-read-{}.txt", uuid::Uuid::new_v4()));
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_read_outside".to_string(),
            tool_name: "Read".to_string(),
            input_summary: outside.to_string_lossy().to_string(),
            input: serde_json::json!({"file_path": outside}),
        };

        let result = can_use_tool_or_queue_permission(
            &tool_use,
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn builtin_permission_tools_allow_without_queueing_prompt() {
        let mut queue = Vec::new();
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_todos".to_string(),
            tool_name: "TodoWrite".to_string(),
            input_summary: "2 todos".to_string(),
            // Must satisfy the tool's own schema — `["content", "status",
            // "activeForm"]`, no `priority`/`id` (see the schema assertion at
            // `tools/todo_write_tool/mod.rs:413-417`). CC reaches
            // `TodoWriteTool.checkPermissions` only after
            // `tool.inputSchema.parse(input)` succeeds
            // (`permissions.ts:1214-1216`); a parse throw leaves the passthrough
            // seed, which step 3 turns into `ask`. So a stale input here does
            // not test the allow at all — it tests the parse failure.
            input: serde_json::json!({
                "todos": [
                    {"content": "inspect", "status": "pending", "activeForm": "Inspecting"}
                ]
            }),
        };

        let result = can_use_tool_or_queue_permission(
            &tool_use,
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let enter_plan = ToolUsePermissionRequest {
            tool_use_id: "toolu_enter_plan".to_string(),
            tool_name: "EnterPlanMode".to_string(),
            input_summary: "{}".to_string(),
            input: serde_json::json!({}),
        };
        let result = can_use_tool_or_queue_permission(
            &enter_plan,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let send_user_message = ToolUsePermissionRequest {
            tool_use_id: "toolu_brief".to_string(),
            tool_name: "SendUserMessage".to_string(),
            input_summary: "hello".to_string(),
            input: serde_json::json!({"message": "hello", "status": "normal"}),
        };
        let result = can_use_tool_or_queue_permission(
            &send_user_message,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let list_mcp_resources = ToolUsePermissionRequest {
            tool_use_id: "toolu_mcp_list".to_string(),
            tool_name: "ListMcpResourcesTool".to_string(),
            input_summary: "{}".to_string(),
            input: serde_json::json!({}),
        };
        let result = can_use_tool_or_queue_permission(
            &list_mcp_resources,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let send_message = ToolUsePermissionRequest {
            tool_use_id: "toolu_send_message".to_string(),
            tool_name: "SendMessage".to_string(),
            input_summary: "to alice".to_string(),
            input: serde_json::json!({"to": "alice", "message": "hello"}),
        };
        let result = can_use_tool_or_queue_permission(
            &send_message,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let task_create = ToolUsePermissionRequest {
            tool_use_id: "toolu_task_create".to_string(),
            tool_name: "TaskCreate".to_string(),
            input_summary: "Create task".to_string(),
            input: serde_json::json!({"subject": "Inspect", "description": "Inspect code"}),
        };
        let result = can_use_tool_or_queue_permission(
            &task_create,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let tool_search = ToolUsePermissionRequest {
            tool_use_id: "toolu_tool_search".to_string(),
            tool_name: "ToolSearch".to_string(),
            input_summary: "select:Read".to_string(),
            // `max_results` is required, not optional: CC's
            // `.optional().default(5)` (`ToolSearchTool.ts:28-32`) puts the
            // default in the outer wrapper, so zod keeps the key required —
            // pinned at `tools/tool_search_tool/mod.rs:487-491`. Omitting it
            // fails the schema check, and a parse failure is CC's passthrough
            // seed, which step 3 turns into `ask`.
            input: serde_json::json!({"query": "select:Read", "max_results": 5}),
        };
        let result = can_use_tool_or_queue_permission(
            &tool_search,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    /// Maps to: CC `tools/TeamCreateTool/TeamCreateTool.ts`, which defines no
    /// `checkPermissions` — `grep -rc checkPermissions tools/TeamCreateTool/`
    /// is 0 — so `buildTool` fills in `TOOL_DEFAULTS.checkPermissions`
    /// (`Tool.ts:762-766`) returning `{behavior:'allow', updatedInput}`, and
    /// that allow is returned verbatim (step 3 at `permissions.ts:1298-1310`,
    /// outer early return at `:486-500`).
    ///
    /// This previously asserted `Ask`, which pinned the invented
    /// `tool_has_builtin_allow_permission` list rather than CC: the list did not
    /// contain TeamCreate, so the tool's own allow was discarded and the port
    /// prompted where CC never does.
    #[test]
    fn team_create_allows_through_tool_defaults_matches_official() {
        let mut queue = Vec::new();
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_team_create".to_string(),
            tool_name: "TeamCreate".to_string(),
            input_summary: "reviewers".to_string(),
            input: serde_json::json!({"team_name": "reviewers"}),
        };

        let result = can_use_tool_or_queue_permission(
            &tool_use,
            &ToolPermissionContext::default(),
            &mut queue,
        );

        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    /// Maps to: CC `tools/ScheduleCronTool/` — `CronCreateTool.ts`,
    /// `CronDeleteTool.ts` and `CronListTool.ts` together contain zero
    /// `checkPermissions` definitions, so all three resolve through
    /// `TOOL_DEFAULTS` (`Tool.ts:762-766`) to allow. There is no
    /// write-prompts/read-allows split in CC.
    ///
    /// The old name and the old `Ask` assertion on CronCreate came from the
    /// invented `tool_has_builtin_allow_permission` list, which happened to
    /// contain only `CronList`. That asymmetry was the list's, not CC's.
    #[test]
    fn cron_create_and_cron_list_both_allow_matches_official() {
        let mut queue = Vec::new();
        let create = ToolUsePermissionRequest {
            tool_use_id: "toolu_cron_create".to_string(),
            tool_name: "CronCreate".to_string(),
            input_summary: "*/5 * * * *".to_string(),
            input: serde_json::json!({"cron": "*/5 * * * *", "prompt": "check deploy"}),
        };
        let result = can_use_tool_or_queue_permission(
            &create,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        queue.clear();
        let list = ToolUsePermissionRequest {
            tool_use_id: "toolu_cron_list".to_string(),
            tool_name: "CronList".to_string(),
            input_summary: "{}".to_string(),
            input: serde_json::json!({}),
        };
        let result =
            can_use_tool_or_queue_permission(&list, &ToolPermissionContext::default(), &mut queue);
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    /// Maps to: CC `tools/RemoteTriggerTool/RemoteTriggerTool.ts`, which defines
    /// no `checkPermissions` — `grep -rc checkPermissions
    /// tools/RemoteTriggerTool/` is 0 — so EVERY action resolves through
    /// `TOOL_DEFAULTS` (`Tool.ts:762-766`) to allow. CC draws no read/write
    /// distinction for this tool.
    ///
    /// The old assertion that `create` prompts came from the invented
    /// `remote_trigger_read_allows` helper, which allowed only `list`/`get`.
    /// That helper was STRICTER than CC, so this port was prompting on actions
    /// the original never asks about.
    #[test]
    fn remote_trigger_allows_every_action_matches_official() {
        let mut queue = Vec::new();
        let list = ToolUsePermissionRequest {
            tool_use_id: "toolu_remote_list".to_string(),
            tool_name: "RemoteTrigger".to_string(),
            input_summary: "list".to_string(),
            input: serde_json::json!({"action": "list"}),
        };
        let result =
            can_use_tool_or_queue_permission(&list, &ToolPermissionContext::default(), &mut queue);
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());

        let create = ToolUsePermissionRequest {
            tool_use_id: "toolu_remote_create".to_string(),
            tool_name: "RemoteTrigger".to_string(),
            input_summary: "create".to_string(),
            input: serde_json::json!({"action": "create", "body": {"name": "nightly"}}),
        };
        let result = can_use_tool_or_queue_permission(
            &create,
            &ToolPermissionContext::default(),
            &mut queue,
        );
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn accept_edits_mode_allows_working_directory_file_edits_without_prompt() {
        let mut queue = Vec::new();
        let mut context = ToolPermissionContext::default();
        context.mode = PermissionMode::AcceptEdits;
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_edit".to_string(),
            tool_name: "Edit".to_string(),
            input_summary: "src/main.rs".to_string(),
            input: serde_json::json!({
                "file_path": "src/main.rs",
                "old_string": "old",
                "new_string": "new"
            }),
        };

        let result = can_use_tool_or_queue_permission(&tool_use, &context, &mut queue);

        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn accept_edits_mode_keeps_sensitive_directories_prompt_required() {
        let mut queue = Vec::new();
        let mut context = ToolPermissionContext::default();
        context.mode = PermissionMode::AcceptEdits;
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu_sensitive_edit".to_string(),
            tool_name: "Edit".to_string(),
            input_summary: ".git/config".to_string(),
            input: serde_json::json!({
                "file_path": ".git/config",
                "old_string": "old",
                "new_string": "new"
            }),
        };

        let result = can_use_tool_or_queue_permission(&tool_use, &context, &mut queue);

        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn deny_rule_rejects_without_queueing_permission_prompt() {
        let mut ctx = ToolPermissionContext::default();
        let mut rules = HashMap::new();
        rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        ctx.always_deny_rules = rules;
        let mut queue = Vec::new();

        let result = can_use_tool_or_queue_permission(&queued_bash(), &ctx, &mut queue);

        assert!(matches!(result, CanUseToolResult::Deny(_)));
        assert!(queue.is_empty());
    }

    #[test]
    fn ask_rule_takes_precedence_over_bypass_mode() {
        let mut ctx = ToolPermissionContext::default();
        ctx.mode = PermissionMode::BypassPermissions;
        let mut rules = HashMap::new();
        rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo test:*".to_string()),
            )],
        );
        ctx.always_ask_rules = rules;
        let mut queue = Vec::new();

        let result = can_use_tool_or_queue_permission(&queued_bash(), &ctx, &mut queue);

        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn bypass_permissions_mode_allows_without_queueing() {
        let mut ctx = ToolPermissionContext::default();
        ctx.mode = PermissionMode::BypassPermissions;
        let mut queue = Vec::new();

        let result = can_use_tool_or_queue_permission(&queued_bash(), &ctx, &mut queue);

        // An allow may carry its matched rule / updatedInput payload (#142);
        // only the variant matters to these queue tests.
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn allowed_rule_does_not_queue_permission() {
        let mut ctx = ToolPermissionContext::default();
        let mut rules = HashMap::new();
        rules.insert(
            PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo test:*".to_string()),
            )],
        );
        ctx.always_allow_rules = rules;
        let mut queue = Vec::new();

        let result = can_use_tool_or_queue_permission(&queued_bash(), &ctx, &mut queue);

        // An allow may carry its matched rule / updatedInput payload (#142);
        // only the variant matters to these queue tests.
        assert!(matches!(result, CanUseToolResult::Allow { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn model_tool_use_block_is_converted_at_boundary() {
        let block = ToolUseBlock {
            id: crate::types::ids::ToolUseId("toolu_model".to_string()),
            name: "Bash".to_string(),
            input: serde_json::json!({ "command": "echo permission-gated" }),
        };

        let tool_use = ToolUsePermissionRequest::from_tool_use_block(&block);
        assert_eq!(tool_use.tool_use_id, "toolu_model");
        assert_eq!(tool_use.tool_name, "Bash");
        assert_eq!(tool_use.input_summary, "echo permission-gated");
    }

    #[test]
    fn transcript_tool_use_is_converted_at_boundary() {
        let message = RenderableMessage::assistant_block(
            "toolu_mock",
            crate::types::message::AssistantContent::ToolUse(crate::types::message::ToolUseBlock {
                id: crate::types::ids::ToolUseId(String::new()),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "echo permission-gated"}),
            }),
        );

        let tool_use = ToolUsePermissionRequest::from_renderable_message(&message)
            .expect("assistant tool_use should convert");
        assert_eq!(tool_use.tool_name, "Bash");
        assert_eq!(tool_use.input_summary, "echo permission-gated");
    }
    #[test]
    fn forced_ask_and_deny_match_official_lossless_decision_transport() {
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "toolu-complete".into(),
            tool_name: "Bash".into(),
            input_summary: "command".into(),
            input: serde_json::json!({"command":"original"}),
        };
        let context = ToolPermissionContext::default();
        let ask = PermissionDecision::Ask {
            message: "ask from tool".into(),
            updated_input: Some(serde_json::json!({"command":"updated"})),
            decision_reason: Some(crate::types::permissions::PermissionDecisionReason::Other {
                reason: "why".into(),
            }),
            suggestions: Vec::new(),
            blocked_path: Some("/restricted".into()),
            metadata: None,
            is_bash_security_check_for_misparsing: true,
            pending_classifier_check: Some(crate::types::permissions::PendingClassifierCheck {
                command: "updated".into(),
                cwd: "/work".into(),
                descriptions: vec!["check".into()],
            }),
            content_blocks: vec![serde_json::json!({"type":"text","text":"context"})],
        };
        let deny = PermissionDecision::Deny {
            message: "denied".into(),
            decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                reason: "why".into(),
            },
            tool_use_id: Some("normalized-tool-id".into()),
        };
        for decision in [ask, deny] {
            let actual = forced_decision_to_can_use_tool_result(
                decision.clone(),
                "Bash",
                &tool_use,
                &context,
            )
            .into_decision()
            .unwrap();
            assert_eq!(actual, decision);
        }
    }

    #[tokio::test]
    async fn canonical_permission_callback_matches_official_abort_rejection() {
        let callback =
            crate::utils::permissions::permissions::has_permissions_to_use_tool_callback();
        let context = crate::tool::ToolUseContext::default();
        context.abort_controller.abort();
        let tool = crate::tools::todo_write_tool::todo_write_tool_schema();
        let input = serde_json::json!({"todos":[]});
        let assistant = AssistantMessage {
            uuid: "assistant".into(),
            timestamp: chrono::Utc::now(),
            content: Vec::new(),
            model: None,
            stop_reason: None,
            usage: None,
        };
        let result = callback
            .decide_async(&tool, &input, &context, &assistant, "toolu", None)
            .await;
        assert_eq!(result, Err(crate::utils::errors::AbortError::default()));
        assert_eq!(
            callback.decide(&tool, &input, &context, &assistant, "toolu", None),
            Err(crate::utils::errors::AbortError::default())
        );
    }

    #[test]
    fn interactive_abort_matches_official_resolved_ask_without_queue_or_forced_allow() {
        // useCanUseTool.tsx:81 / PermissionContext.ts:148-176: abort precedes forceDecision.
        let context = crate::tool::ToolUseContext::default();
        context.abort_controller.abort();
        let input = serde_json::json!({"todos":[]});
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "cancelled".into(),
            tool_name: "TodoWrite".into(),
            input_summary: String::new(),
            input: input.clone(),
        };
        let mut queue = Vec::new();
        let result = can_use_tool_or_queue_permission_for_params_with_store(
            CanUseToolParams::from_tool_use_context(
                &context,
                None,
                &tool_use,
                "cancelled",
                None,
                None,
            ),
            &mut queue,
            None,
        );
        let CanUseToolResult::Ask(request) = result else {
            panic!("interactive cancellation resolves Ask");
        };
        assert_eq!(
            request.message,
            crate::hooks::tool_permission::permission_context::cancel_and_abort_message(
                false, None
            )
        );
        assert!(queue.is_empty());
        let mut headless = context.clone();
        headless
            .tool_permission_context
            .should_avoid_permission_prompts = true;
        let result = can_use_tool(CanUseToolParams::from_tool_use_context(
            &headless,
            None,
            &tool_use,
            "cancelled",
            None,
            None,
        ));
        assert!(matches!(result, CanUseToolResult::Aborted(_)));
    }
    #[test]
    fn cancellation_boundary_matches_official_live_headless_overlay() {
        for overlay in [false, true] {
            let mut state = crate::state::app_state_store::AppState::default();
            std::sync::Arc::make_mut(&mut state.tool_permission_context)
                .should_avoid_permission_prompts = !overlay;
            let store = crate::state::store::AppStore::new(state, None);
            let mut context = crate::tool::ToolUseContext::default().with_app_store(store);
            context
                .tool_permission_context
                .should_avoid_permission_prompts = false;
            context.app_store.avoid_permission_prompts_overlay = overlay;
            context.abort_controller.abort();
            let tool_use = ToolUsePermissionRequest {
                tool_use_id: "abort-live".into(),
                tool_name: "TodoWrite".into(),
                input_summary: String::new(),
                input: serde_json::json!({"todos":[]}),
            };
            let mut queue = Vec::new();
            let result = can_use_tool_or_queue_permission_for_params_with_store(
                CanUseToolParams::from_tool_use_context(
                    &context,
                    None,
                    &tool_use,
                    "abort-live",
                    None,
                    None,
                ),
                &mut queue,
                None,
            );
            assert!(
                matches!(result, CanUseToolResult::Aborted(ref error) if *error == crate::utils::errors::AbortError::default())
            );
            assert!(queue.is_empty());
        }
    }

    #[test]
    fn auto_mode_denial_matches_official_record_then_optional_notification() {
        // CC useCanUseTool.tsx:138-182 records tool.description(), not the
        // Bash command or model prompt, before invoking optional addNotification.
        let mut context = crate::tool::ToolUseContext::default();
        let notifications = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = notifications.clone();
        context.add_notification =
            crate::tool::AddNotification(Some(std::sync::Arc::new(move |notification| {
                assert_eq!(
                    crate::utils::auto_mode_denials::get_auto_mode_denials()[0].display,
                    "Inspect repository"
                );
                captured.lock().unwrap().push(notification);
            })));
        let mut tool_use = queued_bash();
        tool_use.input = serde_json::json!({"command":"pwd", "description":"Inspect repository"});
        let tool = crate::tools::bash_tool::bash_tool_schema();
        let before = chrono::Utc::now().timestamp_millis() as f64;
        let result = can_use_tool(CanUseToolParams::from_tool_use_context(
            &context,
            Some(&tool),
            &tool_use,
            &tool_use.tool_use_id,
            None,
            Some(PermissionDecision::Deny {
                message: "denied".into(),
                decision_reason: PermissionDecisionReason::Classifier {
                    classifier: "auto-mode".into(),
                    reason: "outside scope".into(),
                },
                tool_use_id: None,
            }),
        ));
        assert!(matches!(result, CanUseToolResult::Deny(_)));
        let denials = crate::utils::auto_mode_denials::get_auto_mode_denials();
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0].tool_name, "Bash");
        assert_eq!(denials[0].display, "Inspect repository");
        assert_eq!(denials[0].reason, "outside scope");
        assert!(denials[0].timestamp >= before);
        assert!(denials[0].timestamp <= chrono::Utc::now().timestamp_millis() as f64);
        let notifications = notifications.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].key, "auto-mode-denied");
        assert_eq!(
            notifications[0].priority,
            crate::context::notifications::NotificationPriority::Immediate
        );
        assert_eq!(
            notifications[0].segments[0].color,
            Some(crate::context::notifications::NotificationColor::Error)
        );
        assert_eq!(notifications[0].segments[1].text, " · /permissions");
        assert!(notifications[0].segments[1].dim);
        drop(notifications);
        // Callback absence does not suppress recording; non-auto classifiers
        // must not enter either side effect even when the result is Deny.
        context.add_notification = crate::tool::AddNotification::default();
        for classifier in ["bash", "auto-mode"] {
            can_use_tool(CanUseToolParams::from_tool_use_context(
                &context,
                Some(&tool),
                &tool_use,
                &tool_use.tool_use_id,
                None,
                Some(PermissionDecision::Deny {
                    message: "denied".into(),
                    decision_reason: PermissionDecisionReason::Classifier {
                        classifier: classifier.into(),
                        reason: String::new(),
                    },
                    tool_use_id: None,
                }),
            ));
        }
        assert_eq!(
            crate::utils::auto_mode_denials::get_auto_mode_denials().len(),
            2
        );
        assert!(context.clone().add_notification.0.is_none());
    }

    #[test]
    fn auto_mode_denial_matches_official_abort_before_recording() {
        // CC :81 and :145 exit before the deny side effects on cancellation.
        let context = crate::tool::ToolUseContext::default();
        context.abort_controller.abort();
        let tool_use = queued_bash();
        let result = can_use_tool(CanUseToolParams::from_tool_use_context(
            &context,
            None,
            &tool_use,
            &tool_use.tool_use_id,
            None,
            Some(PermissionDecision::Deny {
                message: "denied".into(),
                decision_reason: PermissionDecisionReason::Classifier {
                    classifier: "auto-mode".into(),
                    reason: "reason".into(),
                },
                tool_use_id: None,
            }),
        ));
        assert!(matches!(result, CanUseToolResult::Ask(_)));
        assert!(crate::utils::auto_mode_denials::get_auto_mode_denials().is_empty());
    }

    #[test]
    fn auto_mode_denial_matches_official_mcp_description_and_user_facing_name() {
        use crate::services::mcp::types::{
            McpClientSnapshot, McpServerConnectionType, McpServerSnapshot, McpToolSnapshot,
        };
        // CC client.ts:1786-1788 supplies untruncated description; :1972-1974
        // supplies the server/title user-facing name used by the notification.
        let raw = "original MCP description ".repeat(250);
        let mut context = crate::tool::ToolUseContext::default();
        context.mcp_state.clients.push(McpServerSnapshot {
            connection_id: None,
            client: McpClientSnapshot {
                name: "docs".into(),
                status: McpServerConnectionType::Connected,
                reconnect_attempt: None,
                max_reconnect_attempts: None,
                ide_name: None,
                server_version: None,
                error: None,
            },
            config: None,
            supports_resources: false,
            tools: vec![McpToolSnapshot {
                name: "read".into(),
                display_name: Some("Read Docs".into()),
                description: Some(raw.clone()),
                input_schema: serde_json::json!({"type":"object"}),
                read_only_hint: true,
                destructive_hint: false,
                open_world_hint: false,
            }],
            prompts: vec![],
            resources: vec![],
        });
        let notifications = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = notifications.clone();
        context.add_notification =
            crate::tool::AddNotification(Some(std::sync::Arc::new(move |notification| {
                captured.lock().unwrap().push(notification)
            })));
        let tool = Tool {
            name: "read".into(), // SDK skip-prefix case still carries mcpInfo.
            description: "truncated model prompt is not the UI description".into(),
            is_mcp: true,
            mcp_info: Some(crate::types::tools::McpToolInfo {
                server_name: "docs".into(),
                tool_name: "read".into(),
            }),
            ..Default::default()
        };
        let tool_use = ToolUsePermissionRequest {
            tool_use_id: "mcp-deny".into(),
            tool_name: "read".into(),
            input_summary: "wrong summary".into(),
            input: serde_json::json!({}),
        };
        can_use_tool(CanUseToolParams::from_tool_use_context(
            &context,
            Some(&tool),
            &tool_use,
            &tool_use.tool_use_id,
            None,
            Some(PermissionDecision::Deny {
                message: "denied".into(),
                decision_reason: PermissionDecisionReason::Classifier {
                    classifier: "auto-mode".into(),
                    reason: "external".into(),
                },
                tool_use_id: None,
            }),
        ));
        let recorded = crate::utils::auto_mode_denials::get_auto_mode_denials();
        assert_eq!(recorded[0].display, raw);
        assert_eq!(recorded[0].tool_name, "read");
        assert_eq!(
            notifications.lock().unwrap()[0].text,
            "docs - read docs (mcp) denied by auto mode · /permissions"
        );
    }
}
