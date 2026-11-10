//! Maps to: CC `Tool.ts` — the Tool interface and its `ToolUseContext`.
//!
//! `ToolUseContext` (CC Tool.ts:158) flows through tool orchestration,
//! execution, hooks, and permission checks. The full CC type carries
//! model/tool options, app-state callbacks, abort controller, notifications,
//! and file caches; this port carries the fields currently exercised by tool
//! execution, `/resume` restore seams, and terminal-notification planning.
//!
//! `ToolCall` below is the behavioral half of CC `Tool` (`call`, Tool.ts:379).
//! The model/API-visible metadata half stays in `crate::types::tools::Tool`
//! (consumed by `utils/api.ts#toolToAPISchema` equivalents).

use crate::types::message::Message;
use crate::types::permissions::{
    AdditionalWorkingDirectory, PermissionMode, ToolPermissionRulesBySource,
};
use crate::types::tools::Tool;
use crate::utils::config::GlobalConfig;
use crate::utils::query_helpers::ReadFileStateEntry;
use crate::utils::session_restore::ResumeRestoreStores;
use crate::utils::terminal_notification::{
    TerminalNotificationEnvironment, TerminalNotificationOptions, TerminalNotificationRequest,
    TerminalNotificationServicePlan, terminal_notification_service_plan_from_config,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Maps to: CC `Tool.ts:123-148#ToolPermissionContext`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionContext {
    pub mode: PermissionMode,
    /// Maps to CC `Tool.ts:125` `additionalWorkingDirectories`: JS Map keeps
    /// insertion order, also observed by runAgent.ts:504-506 prompt assembly.
    #[serde(default)]
    pub additional_working_directories: indexmap::IndexMap<String, AdditionalWorkingDirectory>,
    #[serde(default)]
    pub always_allow_rules: ToolPermissionRulesBySource,
    #[serde(default)]
    pub always_deny_rules: ToolPermissionRulesBySource,
    #[serde(default)]
    pub always_ask_rules: ToolPermissionRulesBySource,
    /// Maps to CC `ToolPermissionContext.isBypassPermissionsModeAvailable`.
    #[serde(default)]
    pub is_bypass_permissions_mode_available: bool,
    /// Maps to CC `ToolPermissionContext.isAutoModeAvailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_auto_mode_available: Option<bool>,
    /// Maps to CC `ToolPermissionContext.strippedDangerousRules`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripped_dangerous_rules: Option<ToolPermissionRulesBySource>,
    /// Maps to CC `ToolPermissionContext.shouldAvoidPermissionPrompts`.
    #[serde(default)]
    pub should_avoid_permission_prompts: bool,
    /// Maps to CC `ToolPermissionContext.awaitAutomatedChecksBeforeDialog`.
    #[serde(default)]
    pub await_automated_checks_before_dialog: bool,
    /// Maps to CC `ToolPermissionContext.prePlanMode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_plan_mode: Option<PermissionMode>,
}

/// Rust trait projection of CC `Tool.ts:150-159#getEmptyToolPermissionContext`.
impl Default for ToolPermissionContext {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            additional_working_directories: indexmap::IndexMap::new(),
            always_allow_rules: HashMap::new(),
            always_deny_rules: HashMap::new(),
            always_ask_rules: HashMap::new(),
            is_bypass_permissions_mode_available: false,
            is_auto_mode_available: None,
            stripped_dangerous_rules: None,
            should_avoid_permission_prompts: false,
            await_automated_checks_before_dialog: false,
            pre_plan_mode: None,
        }
    }
}

/// Owned Rust carrier for CC `ToolUseContext.getAppState` overrides.
///
/// The normal REPL path reads [`crate::state::store::AppStore`]. Forked command
/// contexts can install a projection callback, matching
/// `createGetAppStateWithAllowedTools(baseGetAppState, allowedTools)` without
/// mutating the root store.
#[derive(Clone, Default)]
pub struct GetAppStateCallback(
    pub  Option<
        std::sync::Arc<
            dyn Fn() -> Option<std::sync::Arc<crate::state::app_state_store::AppState>>
                + Send
                + Sync,
        >,
    >,
);

impl GetAppStateCallback {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn() -> Option<std::sync::Arc<crate::state::app_state_store::AppState>>
            + Send
            + Sync
            + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    fn get(&self) -> Option<std::sync::Arc<crate::state::app_state_store::AppState>> {
        self.0.as_ref().and_then(|callback| callback())
    }
}

impl std::fmt::Debug for GetAppStateCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "GetAppStateCallback(set)"
        } else {
            "GetAppStateCallback(unset)"
        })
    }
}

impl PartialEq for GetAppStateCallback {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Live `AppStore` handle for CC `getAppState` / `setAppState`.
///
/// Also carries subagent write-isolation flags so
/// [`QueryParams::tool_use_context`] can rebuild loop-local contexts without
/// losing `createSubagentContext` semantics.
/// Excluded from `PartialEq` (handle identity is not part of tool-context equality).
#[derive(Clone)]
pub struct AppStoreRef {
    pub store: Option<crate::state::store::AppStore>,
    /// Maps to: CC `shareSetAppState` (default true).
    pub writable: bool,
    /// Root store for `setAppStateForTasks`; `None` falls back to [`Self::store`].
    pub tasks_store: Option<crate::state::store::AppStore>,
    /// Maps to: CC async getAppState `shouldAvoidPermissionPrompts` overlay.
    pub avoid_permission_prompts_overlay: bool,
    /// Maps to CC `SubagentContextOverrides.getAppState`.
    pub get_state_override: GetAppStateCallback,
}

impl Default for AppStoreRef {
    fn default() -> Self {
        Self {
            store: None,
            writable: true,
            tasks_store: None,
            avoid_permission_prompts_overlay: false,
            get_state_override: GetAppStateCallback::default(),
        }
    }
}

impl AppStoreRef {
    pub fn new(store: crate::state::store::AppStore) -> Self {
        Self {
            store: Some(store.clone()),
            writable: true,
            tasks_store: Some(store),
            avoid_permission_prompts_overlay: false,
            get_state_override: GetAppStateCallback::default(),
        }
    }

    pub fn is_some(&self) -> bool {
        self.store.is_some() || self.get_state_override.0.is_some()
    }

    /// Maps to: CC `ToolUseContext.setAppState(f)` — the handle-only body of
    /// [`ToolUseContext::set_app_state`], for the seams that hold the store
    /// handles without the surrounding context (the headless permission tail
    /// takes owned values so its future stays `'static`). The `writable` gate
    /// is CC `shareSetAppState`, so both entries share ONE definition of it.
    pub fn set_app_state(&self, f: impl FnOnce(&mut crate::state::app_state_store::AppState)) {
        if !self.writable {
            return;
        }
        if let Some(store) = &self.store {
            store.replace_with(f);
        }
    }
}

impl PartialEq for AppStoreRef {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for AppStoreRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.store.is_some() {
            "AppStoreRef(Some)"
        } else {
            "AppStoreRef(None)"
        })
    }
}

/// Maps to CC `Tool.ts` `QueryChainTracking`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryChainTracking {
    pub chain_id: String,
    pub depth: u32,
}

/// Cooperative cancellation handle for tool execution.
/// Maps to: CC `Tool.ts` `ToolUseContext.abortController` (:180) —
/// `AbortController`/`AbortSignal` semantics reduced to atomic flags;
/// tools poll `is_aborted()` (CC listeners/`signal.aborted`). Child
/// controllers mirror CC `createChildAbortController(...)`: aborting a parent
/// is visible to children, while aborting a child is local.
///
/// Also bridges to SDK `AbortSignal` for `side_query` / classifier
/// (`classifyYoloAction(..., signal)`). One SDK pair is cached per controller
/// (CC: one `abortController.signal` per turn, not per tool call).
#[derive(Clone, Debug)]
pub struct AbortController {
    aborted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// DOM AbortSignal `reason`; the first abort wins. Bash/ShellCommand uses
    /// the official `"interrupt"` distinction to avoid killing block-style
    /// foreground commands when a new queued message arrives.
    reason: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    parents: std::sync::Arc<Vec<AbortController>>,
    /// Cached own SDK pair — [`AbortController::signal`] reuses this for the turn.
    own_sdk: std::sync::Arc<
        std::sync::Mutex<Option<(anthropic_sdk::AbortHandle, anthropic_sdk::AbortSignal)>>,
    >,
    /// Handles that must abort when this controller aborts: own handle +
    /// descendants that registered via parent fan-out.
    linked_handles: std::sync::Arc<std::sync::Mutex<Vec<anthropic_sdk::AbortHandle>>>,
}

impl Default for AbortController {
    fn default() -> Self {
        Self {
            aborted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reason: std::sync::Arc::new(std::sync::Mutex::new(None)),
            parents: std::sync::Arc::new(Vec::new()),
            own_sdk: std::sync::Arc::new(std::sync::Mutex::new(None)),
            linked_handles: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

impl AbortController {
    pub fn child_of(parent: AbortController) -> Self {
        Self {
            aborted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reason: std::sync::Arc::new(std::sync::Mutex::new(None)),
            parents: std::sync::Arc::new(vec![parent]),
            own_sdk: std::sync::Arc::new(std::sync::Mutex::new(None)),
            linked_handles: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub fn abort(&self) {
        self.abort_with_reason("abort");
    }

    /// Maps to DOM `AbortController.abort(reason)` used by CC with
    /// `reason === 'interrupt'` for submit-while-running.
    pub fn abort_with_reason(&self, reason: impl Into<String>) {
        if let Ok(mut slot) = self.reason.lock()
            && slot.is_none()
        {
            *slot = Some(reason.into());
        }
        self.aborted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Poisoned mutex still carries handles we must cancel (Escape mid-classify).
        let handles = self
            .linked_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for handle in handles.iter() {
            handle.abort();
        }
    }

    /// Return this signal's reason, or the first aborted parent reason.
    pub fn reason(&self) -> Option<String> {
        if self.aborted.load(std::sync::atomic::Ordering::SeqCst)
            && let Ok(reason) = self.reason.lock()
            && reason.is_some()
        {
            return reason.clone();
        }
        self.parents.iter().find_map(AbortController::reason)
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(std::sync::atomic::Ordering::SeqCst)
            || self.parents.iter().any(AbortController::is_aborted)
    }

    /// Handle identity check for process-global cancellation owners.
    pub fn same_identity(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.aborted, &other.aborted)
    }

    /// Maps to: CC `AbortController.signal` — the accessor every call site uses
    /// (`AgentTool.tsx:680`, `WebSearchTool.ts:277`, `utils.ts:419`, …).
    /// **One pair per controller.**
    ///
    /// Registers on parents so parent abort cancels this signal (CC AbortSignal
    /// tree: parent abort is visible to child signals).
    ///
    /// Named `sdk_abort_signal` until now, apparently to flag that the return
    /// type comes from `anthropic_sdk`. CC's `.signal` returns a different type
    /// from its owner too (`AbortSignal`, not `AbortController`) and is not
    /// prefixed for it, and this controller has no second signal accessor to
    /// disambiguate against.
    pub fn signal(&self) -> anthropic_sdk::AbortSignal {
        let mut own = self
            .own_sdk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, signal)) = own.as_ref() {
            return signal.clone();
        }

        let (handle, signal) = anthropic_sdk::AbortSignal::pair();
        if self.is_aborted() {
            handle.abort();
        }
        self.register_linked_handle(handle.clone());
        for parent in self.parents.iter() {
            parent.register_linked_handle(handle.clone());
        }
        *own = Some((handle, signal.clone()));
        signal
    }

    fn register_linked_handle(&self, handle: anthropic_sdk::AbortHandle) {
        let mut handles = self
            .linked_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        handles.push(handle);
    }
}

impl PartialEq for AbortController {
    // Contexts compare by the current *observable* abort state — including
    // propagation from `child_of` parents — not by handle identity. Do not
    // use `==` to test whether two handles share the same controller.
    fn eq(&self, other: &Self) -> bool {
        self.is_aborted() == other.is_aborted()
    }
}

/// Cross-thread tool-progress forwarder carried by the context.
/// Rust seam: CC passes `onProgress` per `tool.call(...)` (Tool.ts:383); this
/// port also carries a context-level sink so orchestration threads reach the
/// query-actor event channel without widening every signature.
#[derive(Clone, Default)]
pub struct ToolProgressSink(
    pub Option<std::sync::Arc<dyn Fn(crate::types::tools::ToolProgress) + Send + Sync>>,
);

/// Maps to: CC `Tool.ts:204` `ToolUseContext.addNotification?`.
/// Owned optional callback carrier; presence is independent of AppStore.
#[derive(Clone, Default)]
pub struct AddNotification(
    pub Option<std::sync::Arc<dyn Fn(crate::context::notifications::Notification) + Send + Sync>>,
);

impl std::fmt::Debug for AddNotification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "AddNotification(set)"
        } else {
            "AddNotification(unset)"
        })
    }
}

impl PartialEq for AddNotification {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
    }
}

/// Maps to: CC `Tool.ts:227`
/// `setInProgressToolUseIDs: (f: (prev: Set<string>) => Set<string>) => void`.
///
/// The set itself is REPL state (`REPL.tsx:1897`); `ToolUseContext` carries only
/// the setter, and tool execution calls it on start and finish. Nyan runs
/// tool execution on the query actor, off the render thread, so the installed
/// closure forwards the update over the actor event channel instead of calling
/// `setState` in-process — same role as [`ToolProgressSink`].
///
/// `None` is CC's `setInProgressToolUseIDs: () => {}` no-op, which the official
/// non-REPL contexts install verbatim: `utils/forkedAgent.ts:425`,
/// `utils/queryContext.ts:166`, `utils/hooks/execAgentHook.ts:136`,
/// `QueryEngine.ts:374,522`.
#[derive(Clone, Default)]
pub struct SetInProgressToolUseIds(pub Option<std::sync::Arc<dyn Fn(&str, bool) + Send + Sync>>);

impl std::fmt::Debug for SetInProgressToolUseIds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "SetInProgressToolUseIds(set)"
        } else {
            "SetInProgressToolUseIds(unset)"
        })
    }
}

impl PartialEq for SetInProgressToolUseIds {
    // Contexts compare on setter presence; the closure itself is identity-free.
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to: CC `Tool.ts:224-226` `setHasInterruptibleToolInProgress`.
///
/// `handlePromptSubmit` uses this ref to decide whether an active turn may be
/// interrupted by a new prompt.  The setter is deliberately separate from the
/// in-progress id setter: a tool can be in progress while its
/// `interruptBehavior()` is `block`.
#[derive(Clone, Default)]
pub struct SetHasInterruptibleToolInProgress(
    pub Option<std::sync::Arc<dyn Fn(bool) + Send + Sync>>,
);

impl std::fmt::Debug for SetHasInterruptibleToolInProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "SetHasInterruptibleToolInProgress(set)"
        } else {
            "SetHasInterruptibleToolInProgress(unset)"
        })
    }
}

impl PartialEq for SetHasInterruptibleToolInProgress {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to: CC `ToolUseContext.handleElicitation` (`QueryEngine.ts:153`) —
/// the print/SDK leg that forwards an MCP elicitation to the SDK consumer as
/// an `elicitation` control request (wired from `cli/print.ts:2189`). Empty
/// in REPL mode, where the ElicitationDialog queue serves instead.
#[derive(Clone, Default)]
pub struct HandleElicitationCallback(
    Option<
        std::sync::Arc<
            dyn Fn(
                    String,
                    crate::services::mcp::elicitation_handler::ElicitationRequestParams,
                ) -> BoxFuture<
                    'static,
                    crate::services::mcp::elicitation_handler::ElicitationResult,
                > + Send
                + Sync,
        >,
    >,
);

impl HandleElicitationCallback {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(
                String,
                crate::services::mcp::elicitation_handler::ElicitationRequestParams,
            )
                -> BoxFuture<'static, crate::services::mcp::elicitation_handler::ElicitationResult>
            + Send
            + Sync
            + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    /// The registered handler, when the print/SDK leg is wired.
    #[allow(clippy::type_complexity)]
    pub fn get(
        &self,
    ) -> Option<
        &std::sync::Arc<
            dyn Fn(
                    String,
                    crate::services::mcp::elicitation_handler::ElicitationRequestParams,
                ) -> BoxFuture<
                    'static,
                    crate::services::mcp::elicitation_handler::ElicitationResult,
                > + Send
                + Sync,
        >,
    > {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for HandleElicitationCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "HandleElicitationCallback(set)"
        } else {
            "HandleElicitationCallback(unset)"
        })
    }
}

impl PartialEq for HandleElicitationCallback {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

// Mirrors CC `Tool.ts:12` `import type { CanUseToolFn } from
// './hooks/useCanUseTool.js'` — the owned carrier for that fn type is declared
// in `hooks/use_can_use_tool.rs` (the file mapped to the CC file that declares
// it); `tool.rs` only imports it for `ToolUseContext.can_use_tool` (#156
// placement fix, same rule as task #162).
pub use crate::hooks::use_can_use_tool::CanUseToolCallback;

/// Rust representation of the official `ToolUseContext.setToolJSX` path used
/// by `spawnMultiAgent.ts` to mount `utils/swarm/It2SetupPrompt.tsx` and await
/// its `onDone` result. The generic ReactNode payload is represented by the
/// one official prompt currently consumed from tool execution.
#[derive(Clone, Default)]
pub struct It2SetupPromptSink(
    pub  Option<
        std::sync::Arc<
            dyn Fn(
                    bool,
                ) -> BoxFuture<
                    'static,
                    crate::utils::swarm::it2_setup_prompt::It2SetupPromptResult,
                > + Send
                + Sync,
        >,
    >,
);

impl std::fmt::Debug for ToolProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ToolProgressSink(set)"
        } else {
            "ToolProgressSink(unset)"
        })
    }
}

impl PartialEq for ToolProgressSink {
    // Contexts compare on sink presence; the closure itself is identity-free.
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

impl std::fmt::Debug for It2SetupPromptSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "It2SetupPromptSink(set)"
        } else {
            "It2SetupPromptSink(unset)"
        })
    }
}

impl PartialEq for It2SetupPromptSink {
    // Contexts compare on sink presence; the closure itself is identity-free.
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// What an asker hands the leader's dialog queue.
///
/// Maps to: CC `interactiveHandler.ts:92-100` `ctx.pushToQueue({
/// assistantMessage, tool, description, input, toolUseContext, toolUseID,
/// permissionResult, permissionPromptStartTimeMs, ... })` and the
/// byte-equivalent `inProcessRunner.ts:225-236` (which adds `workerBadge`).
/// CC needs no such struct because its asker owns `setToolUseConfirmQueue`
/// directly; this port's asker reaches the queue through
/// [`InteractivePermissionSink`], so the payload has to travel.
///
/// `request` folds CC's `tool` / `input` / `description` / `toolUseID` /
/// `permissionResult` (see [`crate::types::permissions::PermissionRequest`]);
/// `permissionPromptStartTimeMs` is stamped at the push
/// (`interactive_handler.rs#tool_use_confirm_for_request`); CC's
/// `assistantMessage` is used only for `logPermissionDecision`'s `messageId`
/// (`PermissionContext.ts:105,120-130`), which this port logs from the query
/// actor instead.
#[derive(Clone, Debug)]
pub struct InteractivePermissionAsk {
    pub request: crate::types::permissions::PermissionRequest,
    /// Maps to: CC `inProcessRunner.ts:234-236` `workerBadge`.
    pub worker_badge: Option<crate::types::permissions::PermissionWorkerBadge>,
    /// Maps to: CC `toolUseContext: ctx.toolUseContext` on the pushed row.
    /// See [`crate::types::permissions::ToolUseConfirm::asking_tool_permission_context`]
    /// for why the permission-context slice is the complete carrier.
    pub asking_tool_permission_context: Option<ToolPermissionContext>,
}

impl InteractivePermissionAsk {
    pub fn new(request: crate::types::permissions::PermissionRequest) -> Self {
        Self {
            request,
            worker_badge: None,
            asking_tool_permission_context: None,
        }
    }

    pub fn with_worker_badge(
        mut self,
        worker_badge: Option<crate::types::permissions::PermissionWorkerBadge>,
    ) -> Self {
        self.worker_badge = worker_badge;
        self
    }

    pub fn with_asking_tool_permission_context(
        mut self,
        context: Option<ToolPermissionContext>,
    ) -> Self {
        self.asking_tool_permission_context = context;
        self
    }
}

/// The dialog half of CC's `canUseTool`, carried on the context.
///
/// Maps to: CC `hooks/useCanUseTool.tsx:189-327` `case 'ask':` →
/// `handleInteractivePermission({ ctx, ... }, resolve)` — the leg that pushes a
/// `ToolUseConfirm` onto the REPL's queue and leaves the `canUseTool` promise
/// pending until `onAllow`/`onReject` fires.
///
/// CC needs no carrier: `canUseTool` is literally the parent REPL's closure,
/// handed to a subagent as a call parameter (`AgentTool.tsx:399` → `:879` →
/// `runAgent.ts:753`). This port evaluates permissions inside each query actor
/// and raises the dialog by emitting `QueryEvent::PermissionRequest` on that
/// actor's own event stream, which the REPL drains only for the query it is
/// driving — a subagent's actor has no route there. This sink is that route:
/// the REPL installs it on the context it hands to `query(...)`
/// (`screens/repl.rs#apply_repl_tool_use_context_options`), and the entry
/// carries its own `onAllow`/`onReject` so the answer returns to the asking
/// query rather than to `active_query`.
///
/// `None` means no dialog leg is attached: tests, and print/SDK, where CC's
/// `canUseTool` is `hasPermissionsToUseTool` with nothing behind it
/// (`cli/print.ts:4276-4293`, and `:4654-4655` "print.ts never calls
/// handleInteractivePermission"). Callers must fall back to CC's no-prompt
/// behaviour, not block.
#[derive(Clone, Default)]
pub struct InteractivePermissionSink(
    #[allow(clippy::type_complexity)]
    Option<
        std::sync::Arc<
            dyn Fn(
                    InteractivePermissionAsk,
                ) -> BoxFuture<
                    'static,
                    Option<crate::types::permissions::PermissionPromptResponse>,
                > + Send
                + Sync,
        >,
    >,
);

impl InteractivePermissionSink {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(
                InteractivePermissionAsk,
            )
                -> BoxFuture<'static, Option<crate::types::permissions::PermissionPromptResponse>>
            + Send
            + Sync
            + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }

    /// Raise the dialog and await the user's answer. `None` when no sink is
    /// installed, or when the REPL dropped the request without answering.
    pub async fn ask(
        &self,
        ask: InteractivePermissionAsk,
    ) -> Option<crate::types::permissions::PermissionPromptResponse> {
        let callback = self.0.as_ref()?;
        callback(ask).await
    }
}

impl std::fmt::Debug for InteractivePermissionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "InteractivePermissionSink(set)"
        } else {
            "InteractivePermissionSink(unset)"
        })
    }
}

impl PartialEq for InteractivePermissionSink {
    // Contexts compare on sink presence; the closure itself is identity-free.
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to CC `ToolUseContext.setResponseLength` for subagent token-display
/// contribution. Rust callers report the positive UTF-16 delta used by CC.
#[derive(Clone, Default)]
pub struct ResponseLengthSink(pub Option<std::sync::Arc<dyn Fn(usize) + Send + Sync>>);

impl ResponseLengthSink {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    pub fn add(&self, delta: usize) {
        if let Some(callback) = &self.0 {
            callback(delta);
        }
    }
}

impl std::fmt::Debug for ResponseLengthSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ResponseLengthSink(set)"
        } else {
            "ResponseLengthSink(unset)"
        })
    }
}

impl PartialEq for ResponseLengthSink {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to CC `ToolUseContext.setStreamMode`; optional in hook contexts.
#[derive(Clone, Default)]
pub struct StreamModeSink(pub Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>);
impl StreamModeSink {
    pub fn set(&self, mode: &str) {
        if let Some(callback) = &self.0 {
            callback(mode);
        }
    }
}
impl std::fmt::Debug for StreamModeSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamModeSink")
    }
}
impl PartialEq for StreamModeSink {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to CC `ToolUseContext.pushApiMetricsEntry`.
#[derive(Clone, Default)]
pub struct ApiMetricsSink(pub Option<std::sync::Arc<dyn Fn(u64) + Send + Sync>>);

impl ApiMetricsSink {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    pub fn push(&self, ttft_ms: u64) {
        if let Some(callback) = &self.0 {
            callback(ttft_ms);
        }
    }
}

impl std::fmt::Debug for ApiMetricsSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ApiMetricsSink(set)"
        } else {
            "ApiMetricsSink(unset)"
        })
    }
}

impl PartialEq for ApiMetricsSink {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Maps to CC `ToolUseContext.options.refreshTools`.
#[derive(Clone, Default)]
pub struct RefreshToolsCallback(
    pub Option<std::sync::Arc<dyn Fn() -> Vec<crate::types::tools::Tool> + Send + Sync>>,
);

impl RefreshToolsCallback {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn() -> Vec<crate::types::tools::Tool> + Send + Sync + 'static,
    {
        Self(Some(std::sync::Arc::new(callback)))
    }

    pub fn refresh(&self) -> Option<Vec<crate::types::tools::Tool>> {
        self.0.as_ref().map(|callback| callback())
    }
}

impl std::fmt::Debug for RefreshToolsCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "RefreshToolsCallback(set)"
        } else {
            "RefreshToolsCallback(unset)"
        })
    }
}

impl PartialEq for RefreshToolsCallback {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// Flattened Rust projection of CC `ToolUseContext.options` used by
/// `SubagentContextOverrides.options`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolUseContextOptions {
    pub commands: std::sync::Arc<Vec<crate::commands::Command>>,
    pub debug: bool,
    pub verbose: bool,
    pub main_loop_model: Option<String>,
    /// Profile `main_loop_model` belongs to; `None` is the session's main profile.
    pub main_loop_profile: Option<String>,
    pub tools: Vec<crate::types::tools::Tool>,
    pub thinking_config: Option<crate::utils::thinking::ThinkingConfig>,
    pub mcp_state: crate::state::app_state_store::McpState,
    pub is_non_interactive_session: bool,
    pub agent_definitions:
        std::sync::Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    pub max_budget_usd: Option<f64>,
    /// Maps to CC headless `fallbackModel` carried into API options.
    pub fallback_model: Option<String>,
    pub custom_system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub query_source: Option<crate::constants::query_source::QuerySource>,
    pub refresh_tools: RefreshToolsCallback,
}

/// Maps to CC `ToolUseContext.fileReadingLimits` (`Tool.ts:251-254`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileReadingLimitsOverride {
    pub max_tokens: Option<f64>,
    pub max_size_bytes: Option<f64>,
}

/// Maps to CC `ToolUseContext.globLimits`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobLimits {
    pub max_results: Option<usize>,
}

/// Maps to CC `Tool.isSearchOrReadCommand(...)` return value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOrReadCommand {
    pub is_search: bool,
    pub is_read: bool,
}

/// Maps to CC `ToolUseContext.toolDecisions` entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDecision {
    pub source: String,
    pub decision: String,
    pub timestamp_ms: i64,
}

/// L1 (`FileReadTool fallible source-position state carrier`): CC one
/// JavaScript event-loop turn ≙ one recovered process-wide Rust mutex turn.
/// This gate owns scheduling only; cache and ordered-Set policy remain in
/// their source-shaped owners.
pub struct FileReadSourceTurn;

static FILE_READ_SOURCE_TURN: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
std::thread_local! {
    static FILE_READ_SOURCE_TURN_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl FileReadSourceTurn {
    pub(crate) fn run<R>(operation: impl FnOnce() -> R) -> R {
        // Contract A clause 6 acquisition order: holding the FileRead gate
        // may enter the store turn (nested), never the reverse — entering
        // this gate while holding the store turn risks AB-BA deadlock.
        debug_assert!(
            !crate::state::store::store_turn_held_by_current_thread(),
            "FileRead gate entered while holding the AppStore turn — either a clause-6 \
             ordering violation (store turn → FileRead gate) or a FileRead re-entry \
             from inside a store-turn effect (itself the re-entry seam below)"
        );
        if FILE_READ_SOURCE_TURN_ACTIVE.with(std::cell::Cell::get) {
            panic!("synchronous FileRead source-turn re-entry is an explicit partial seam");
        }
        let _turn = FILE_READ_SOURCE_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct ResetActive;
        impl Drop for ResetActive {
            fn drop(&mut self) {
                FILE_READ_SOURCE_TURN_ACTIVE.with(|active| active.set(false));
            }
        }
        FILE_READ_SOURCE_TURN_ACTIVE.with(|active| active.set(true));
        let _reset = ResetActive;
        operation()
    }
}

/// L1 (`FileReadTool fallible source-position state carrier`): CC independently
/// shallow-shared `readFileState` ≙ an Arc-shared short critical section.
#[derive(Clone)]
pub struct SharedFileStateCache(
    std::sync::Arc<std::sync::Mutex<crate::utils::file_state_cache::FileStateCache>>,
);

impl Default for SharedFileStateCache {
    fn default() -> Self {
        Self::fresh()
    }
}

impl SharedFileStateCache {
    pub(crate) fn fresh() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            crate::utils::file_state_cache::FileStateCache::default(),
        )))
    }

    pub(crate) fn from_entries(entries: Vec<ReadFileStateEntry>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            crate::utils::file_state_cache::FileStateCache::from_entries(entries),
        )))
    }

    pub(crate) fn from_snapshot(
        snapshot: crate::utils::file_state_cache::FileStateCacheSnapshot,
    ) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            crate::utils::file_state_cache::FileStateCache::from_snapshot(snapshot),
        )))
    }

    pub(crate) fn cloned_contents(&self) -> Self {
        Self::from_snapshot(self.cache_snapshot())
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn with_cache_in_source_turn<R>(
        &self,
        operation: impl FnOnce(&mut crate::utils::file_state_cache::FileStateCache) -> R,
    ) -> R {
        let mut cache = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut cache)
    }

    pub(crate) fn cache_snapshot(&self) -> crate::utils::file_state_cache::FileStateCacheSnapshot {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.snapshot()))
    }

    pub(crate) fn snapshot(&self) -> Vec<ReadFileStateEntry> {
        FileReadSourceTurn::run(|| {
            self.with_cache_in_source_turn(|cache| cache.entries_lru_to_mru())
        })
    }

    pub(crate) fn get(&self, path: &std::path::Path) -> Option<ReadFileStateEntry> {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.get(path)))
    }

    pub(crate) fn set(&self, path: &std::path::Path, entry: ReadFileStateEntry) {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.set(path, entry)));
    }

    pub(crate) fn set_entry(&self, entry: ReadFileStateEntry) {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.set_entry(entry)));
    }

    pub(crate) fn has(&self, path: &std::path::Path) -> bool {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.has(path)))
    }

    pub(crate) fn delete(&self, path: &std::path::Path) -> bool {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.delete(path)))
    }

    pub(crate) fn clear(&self) {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.clear()));
    }

    pub(crate) fn replace(&self, entries: Vec<ReadFileStateEntry>) {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.replace(entries)));
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.keys()))
    }

    pub(crate) fn len(&self) -> usize {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.len()))
    }

    pub(crate) fn is_empty(&self) -> bool {
        FileReadSourceTurn::run(|| self.with_cache_in_source_turn(|cache| cache.is_empty()))
    }
}

impl std::fmt::Debug for SharedFileStateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cache_snapshot().fmt(f)
    }
}

impl PartialEq for SharedFileStateCache {
    fn eq(&self, other: &Self) -> bool {
        self.same_identity(other) || self.cache_snapshot() == other.cache_snapshot()
    }
}

impl Eq for SharedFileStateCache {}

/// L1 (`FileReadTool fallible source-position state carrier`): CC independently
/// shallow-shared insertion-ordered Set ≙ an Arc-shared short critical section.
#[derive(Clone)]
pub struct SharedOrderedTriggerSet(std::sync::Arc<std::sync::Mutex<indexmap::IndexSet<String>>>);

impl Default for SharedOrderedTriggerSet {
    fn default() -> Self {
        Self::fresh()
    }
}

impl SharedOrderedTriggerSet {
    pub(crate) fn fresh() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            indexmap::IndexSet::new(),
        )))
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn with_set_in_source_turn<R>(
        &self,
        operation: impl FnOnce(&mut indexmap::IndexSet<String>) -> R,
    ) -> R {
        let mut set = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut set)
    }

    pub(crate) fn add(&self, value: String) -> bool {
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.insert(value)))
    }

    pub(crate) fn extend(&self, values: impl IntoIterator<Item = String>) {
        let values = values.into_iter().collect::<Vec<_>>();
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.extend(values)));
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        FileReadSourceTurn::run(|| {
            self.with_set_in_source_turn(|set| set.iter().cloned().collect())
        })
    }

    pub(crate) fn clear(&self) {
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.clear()));
    }

    pub(crate) fn is_empty(&self) -> bool {
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.is_empty()))
    }

    pub(crate) fn contains(&self, value: &str) -> bool {
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.contains(value)))
    }

    pub(crate) fn len(&self) -> usize {
        FileReadSourceTurn::run(|| self.with_set_in_source_turn(|set| set.len()))
    }

    /// L1 live iterator step for CC `for (const path of set) ...; set.clear()`.
    pub(crate) fn next_or_clear(&self, index: usize) -> Option<String> {
        FileReadSourceTurn::run(|| {
            self.with_set_in_source_turn(|set| {
                if let Some(value) = set.get_index(index).cloned() {
                    Some(value)
                } else {
                    set.clear();
                    None
                }
            })
        })
    }
}

impl std::fmt::Debug for SharedOrderedTriggerSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.snapshot().fmt(f)
    }
}

impl PartialEq for SharedOrderedTriggerSet {
    fn eq(&self, other: &Self) -> bool {
        self.same_identity(other) || self.snapshot() == other.snapshot()
    }
}

impl Eq for SharedOrderedTriggerSet {}

/// Maps to: CC `ToolUseContext.localDenialTracking` (`Tool.ts:280-283`).
///
/// Shared and mutable because CC's is a plain object reference: `persistDenialState`
/// writes it with `Object.assign(context.localDenialTracking, newState)`
/// (`utils/permissions/permissions.ts:967-968`), and `createSubagentContext` hands
/// the SAME object to a sharing fork while giving an isolated one its own
/// `createDenialTrackingState()` (`utils/forkedAgent.ts:420-422`).
///
/// A plain `Option<DenialTrackingState>` cannot express either half: the state is
/// `Copy`, so the sharing branch would fork the counter, and the permission path
/// only ever holds `&ToolUseContext`, so it could not write one back at all. That
/// is why every call site passed `None` and an isolated subagent's denials landed
/// in the parent's store.
///
/// Same carrier shape as [`SharedOrderedTriggerSet`] above.
#[derive(Clone)]
pub struct SharedDenialTracking(
    std::sync::Arc<
        std::sync::Mutex<crate::utils::permissions::denial_tracking::DenialTrackingState>,
    >,
);

impl SharedDenialTracking {
    pub fn new(state: crate::utils::permissions::denial_tracking::DenialTrackingState) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(state)))
    }

    /// CC reads the object at each use (`permissions.ts:490`, `:556`).
    pub fn get(&self) -> crate::utils::permissions::denial_tracking::DenialTrackingState {
        *self.0.lock().expect("denial tracking lock poisoned")
    }

    /// CC `Object.assign(context.localDenialTracking, newState)` (`:967-968`) —
    /// an in-place write every sharer observes.
    pub fn set(&self, state: crate::utils::permissions::denial_tracking::DenialTrackingState) {
        *self.0.lock().expect("denial tracking lock poisoned") = state;
    }

    fn same_identity(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for SharedDenialTracking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedDenialTracking")
            .field(&self.get())
            .finish()
    }
}

impl PartialEq for SharedDenialTracking {
    fn eq(&self, other: &Self) -> bool {
        self.same_identity(other) || self.get() == other.get()
    }
}

impl Eq for SharedDenialTracking {}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolUseContext {
    /// Maps to: CC `ToolUseContext.getAppState` / `setAppState` — live store
    /// handle (plus subagent write-isolation flags). Snapshots below are
    /// hydrated from / committed to this store when present; tests and
    /// headless paths may leave it empty.
    pub app_store: AppStoreRef,
    /// Maps to: CC `ToolUseContext.abortController` (Tool.ts:180).
    pub abort_controller: AbortController,
    /// Rust seam: see [`ToolProgressSink`].
    pub tool_progress_sink: ToolProgressSink,
    /// Maps to: CC `Tool.ts:204` optional notification provider callback.
    pub add_notification: AddNotification,
    /// Maps to CC `ToolUseContext.setResponseLength` / `pushApiMetricsEntry`.
    pub response_length_sink: ResponseLengthSink,
    pub stream_mode_sink: StreamModeSink,
    pub api_metrics_sink: ApiMetricsSink,
    /// Maps to CC `ToolUseContext.setToolJSX` when `spawnMultiAgent.ts` needs
    /// to mount `It2SetupPrompt` and hide the prompt input.
    pub it2_setup_prompt_sink: It2SetupPromptSink,
    /// Rust carrier for the dialog half of CC's `canUseTool` — see
    /// [`InteractivePermissionSink`].
    pub interactive_permission_sink: InteractivePermissionSink,
    /// Maps to CC `ToolUseContext.getAppState().channelPermissionCallbacks`
    /// consumed by `hooks/toolPermission/handlers/interactiveHandler.ts`.
    pub channel_permission_callbacks:
        Option<crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    /// Query-owned permission callback. Maps to CC `QueryParams.canUseTool`;
    /// carrying it on the cloned Rust context keeps it available to both the
    /// normal and streaming orchestration paths.
    pub can_use_tool: CanUseToolCallback,
    /// Maps to: CC `ToolUseContext.handleElicitation` — see
    /// [`HandleElicitationCallback`].
    pub handle_elicitation: HandleElicitationCallback,
    /// Maps to CC `QueryParams.skipCacheWrite`. This actor-transport field is
    /// copied into API `Options.skip_cache_write` for every fork iteration.
    pub skip_cache_write: bool,
    /// Maps to CC `QueryParams.maxOutputTokensOverride` for fork consumers.
    pub max_output_tokens_override: Option<u32>,
    /// Maps to `getAppState().toolPermissionContext` / permission-related
    /// fields consumed by official `useCanUseTool`.
    pub tool_permission_context: ToolPermissionContext,
    /// Maps to official `ToolUseContext.messages`, updated by `query.ts` after
    /// compact/collapse and before `runTools(...)`.
    pub messages: Vec<Message>,
    /// Remaining fields in CC `ToolUseContext.options` that are not already
    /// represented by the model/tools/thinking/MCP fields below.
    pub commands: std::sync::Arc<Vec<crate::commands::Command>>,
    pub debug: bool,
    pub verbose: bool,
    pub agent_definitions:
        std::sync::Arc<crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult>,
    pub max_budget_usd: Option<f64>,
    /// Maps to CC `QueryEngine.ask(..., fallbackModel)` for SDK/headless turns.
    pub fallback_model: Option<String>,
    pub custom_system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub query_source_override: Option<crate::constants::query_source::QuerySource>,
    pub refresh_tools: RefreshToolsCallback,
    /// Maps to official `ToolUseContext.options.tools`, the active tool
    /// definitions used by orchestration, permission checks, and execution.
    pub tools: Vec<Tool>,
    /// Maps to official `ToolUseContext.options.mainLoopModel`, consumed by
    /// compact/autocompact thresholds and nested agent runtime options.
    pub main_loop_model: Option<String>,
    /// Profile `main_loop_model` belongs to; `None` is the session's main profile.
    pub main_loop_profile: Option<String>,
    /// Maps to official `ToolUseContext.options.thinkingConfig`.
    pub thinking_config: Option<crate::utils::thinking::ThinkingConfig>,
    /// Maps to CC `AppState.effortValue` as observed by
    /// `runAgent.ts#createSubagentContext` for nested agent effort fallback.
    pub effort_value: Option<crate::utils::effort::Effort>,
    /// Maps to CC `ToolUseContext.getAppState().mcp`: live MCP clients,
    /// discovered tools, prompts, and resources used by dynamic MCP tools.
    pub mcp_state: crate::state::app_state_store::McpState,
    /// Maps to official `ToolUseContext.options.isNonInteractiveSession`.
    /// Interactive REPL turns keep this false; SDK/headless callers can carry
    /// it through this context without changing query or tool execution
    /// responsibilities.
    pub is_non_interactive_session: bool,
    /// Maps to CC `ToolUseContext.agentId` for agent-scoped queue draining
    /// (`utils/attachments.ts#getAgentPendingMessageAttachments`).
    pub agent_id: Option<String>,
    /// Maps to CC `ToolUseContext.agentType` and `requireCanUseTool`.
    pub agent_type: Option<String>,
    pub require_can_use_tool: bool,
    pub preserve_tool_use_results: bool,
    /// Subagent nesting depth (main session = 0, child subagent = parent depth + 1).
    pub subagent_depth: usize,
    /// Last successfully validated StructuredOutput payload for headless/SDK
    /// result projection. Out-of-band: never enters Anthropic prompt content.
    pub structured_output: Option<serde_json::Value>,
    /// Maps to CC `utils/cwd.ts#runWithCwdOverride` / `getCwd`: nested agent
    /// execution can carry an async-context working directory override without
    /// mutating the process-global cwd.
    pub cwd_override: Option<std::path::PathBuf>,
    /// Maps to official `ToolUseContext.queryTracking`, attached by
    /// `query.ts` once per query-loop iteration and carried into tool
    /// execution, hooks, and analytics.
    pub query_tracking: Option<QueryChainTracking>,
    /// Execution-local mirror of the REPL-owned in-progress set, kept so
    /// orchestration/execution can read live tool-use state without a round
    /// trip. Authority is [`Self::set_in_progress_tool_use_ids`]; this field is
    /// only ever written through `mark_in_progress` / `mark_complete`.
    pub in_progress_tool_use_ids: HashSet<String>,
    /// Maps to: CC `Tool.ts:227` `setInProgressToolUseIDs`.
    pub set_in_progress_tool_use_ids: SetInProgressToolUseIds,
    /// Maps to: CC `Tool.ts:224-226` `setHasInterruptibleToolInProgress`.
    pub set_has_interruptible_tool_in_progress: SetHasInterruptibleToolInProgress,
    /// Rust execution-local projection of the tools whose interrupt behavior is
    /// `cancel`.  CC keeps this as `StreamingToolExecutor` state; the context
    /// carries the same state across Rust's cloned actor snapshots.
    pub interruptible_tool_use_ids: HashSet<String>,
    /// Maps to: CC `Tool.ts:215-223` optional trigger Sets.
    /// L1 (`FileReadTool fallible source-position state carrier`): each Set
    /// keeps an independently replaceable shallow-shared identity; `None` is
    /// distinct from a present empty Set.
    pub nested_memory_attachment_triggers: Option<SharedOrderedTriggerSet>,
    pub loaded_nested_memory_paths: HashSet<String>,
    pub dynamic_skill_dir_triggers: Option<SharedOrderedTriggerSet>,
    pub discovered_skill_names: HashSet<String>,
    pub tool_decisions: Option<std::collections::HashMap<String, ToolDecision>>,
    pub user_modified: Option<bool>,
    /// Maps to CC `Tool.ts:274` `ToolUseContext.toolUseId` — the id of the
    /// tool_use block this call executes. Set per call by the dispatcher
    /// (`toolExecution.ts:1211` spreads `{...toolUseContext, toolUseId,
    /// userModified}` into `tool.call`); `None` outside a tool call.
    pub tool_use_id: Option<String>,
    /// Maps to CC `ToolUseContext.localDenialTracking` (`Tool.ts:280-283`) —
    /// async subagents whose `setAppState` is a no-op keep a local denial
    /// counter so retries still accumulate (see
    /// `utils/permissions/denialTracking.ts`).
    ///
    /// Shared and mutable, because CC's is: `persistDenialState` writes it with
    /// `Object.assign(context.localDenialTracking, newState)`
    /// (`utils/permissions/permissions.ts:967-968`) — an in-place mutation of an
    /// object the context only holds a reference to — and
    /// `createSubagentContext` hands the SAME object to a sharing fork
    /// (`utils/forkedAgent.ts:420-422`). A plain owned `DenialTrackingState`
    /// would be `Copy`, so the sharing branch would silently fork the counter,
    /// and the permission path only ever holds `&ToolUseContext` so it could
    /// not write one back at all.
    pub local_denial_tracking: Option<SharedDenialTracking>,
    /// Maps to CC `ToolUseContext.fileReadingLimits`; compact/fork consumers
    /// can override either cap independently while inheriting the other.
    pub file_reading_limits: Option<FileReadingLimitsOverride>,
    /// Maps to CC `ToolUseContext.globLimits`; absent in normal interactive
    /// turns, but agent/SDK callers may lower the Glob result count.
    pub glob_limits: Option<GlobLimits>,
    /// Maps to: CC `Tool.ts:181` `ToolUseContext.readFileState`.
    /// L1 (`FileReadTool fallible source-position state carrier`): ordinary
    /// context clones preserve this independent shallow-shared cache identity.
    pub read_file_state: SharedFileStateCache,
    /// Read-only resume payload seam for official `bashTools.current`.
    pub bash_tools: Vec<String>,
    /// Maps to CC `ToolUseContext.contentReplacementState`, used by subagent
    /// context creation to inherit or reset stable tool-result replacement
    /// decisions for prompt-cache-safe repeated context construction.
    pub content_replacement_state:
        Option<crate::utils::tool_result_storage::ContentReplacementState>,
    /// Parent's rendered system prompt bytes, frozen at turn start.
    /// Used by fork subagents to share the parent's prompt cache — re-calling
    /// getSystemPrompt() at fork-spawn time can diverge (GrowthBook cold→warm)
    /// and bust the cache. See forkSubagent.ts.
    ///
    /// Maps to: CC `Tool.ts:293-299` `renderedSystemPrompt?: SystemPrompt`.
    /// Written by the per-turn REPL choke point
    /// (`screens/repl.rs#apply_repl_query_turn_context`, CC `REPL.tsx:3750`);
    /// read by fork resume (`resumeAgent.ts:118-119`) and fork spawn
    /// (`AgentTool.tsx:728-729`).
    pub rendered_system_prompt: Option<crate::utils::system_prompt_type::SystemPrompt>,
    /// Maps to CC `ToolUseContext.criticalSystemReminder_EXPERIMENTAL`, consumed
    /// by `utils/attachments.ts#getCriticalSystemReminderAttachment` for agents
    /// that re-inject short safety/task reminders at each continuation.
    pub critical_system_reminder_experimental: Option<String>,
    /// Full UI-safe `/resume` restore-store snapshot. This lets future query
    /// and tool ports consume official restore boundaries (file history,
    /// context-collapse, TodoWrite, skill, and metadata state) without session
    /// writes, backup copies, hooks, cwd/worktree mutation, or real tool I/O.
    pub resume_restore_stores: ResumeRestoreStores,
    /// UI-only counterpart of official `sendOSNotification`. These requests are
    /// captured in memory so future tool ports can share the official boundary
    /// without emitting terminal OSC/BEL side effects by default.
    pub terminal_notification_requests: Vec<TerminalNotificationRequest>,
    /// Service-level notification plans shaped after official
    /// `services/notifier.ts#sendNotification`: readonly config channel,
    /// Notification hook input, and method-used metadata. Hooks, analytics, and
    /// raw terminal writes remain intentionally unexecuted in this safety phase.
    pub terminal_notification_service_plans: Vec<TerminalNotificationServicePlan>,
}

impl Default for ToolUseContext {
    fn default() -> Self {
        Self::with_permission_context(ToolPermissionContext::default())
    }
}

impl ToolUseContext {
    pub fn with_permission_context(tool_permission_context: ToolPermissionContext) -> Self {
        Self {
            app_store: AppStoreRef::default(),
            abort_controller: AbortController::default(),
            tool_progress_sink: ToolProgressSink::default(),
            add_notification: AddNotification::default(),
            response_length_sink: ResponseLengthSink::default(),
            stream_mode_sink: StreamModeSink::default(),
            api_metrics_sink: ApiMetricsSink::default(),
            it2_setup_prompt_sink: It2SetupPromptSink::default(),
            interactive_permission_sink: InteractivePermissionSink::default(),
            channel_permission_callbacks: None,
            can_use_tool: CanUseToolCallback::default(),
            handle_elicitation: HandleElicitationCallback::default(),
            skip_cache_write: false,
            max_output_tokens_override: None,
            commands: std::sync::Arc::new(Vec::new()),
            debug: false,
            verbose: false,
            agent_definitions: std::sync::Arc::new(
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult::default(),
            ),
            max_budget_usd: None,
            fallback_model: None,
            custom_system_prompt: None,
            append_system_prompt: None,
            query_source_override: None,
            refresh_tools: RefreshToolsCallback::default(),
            tools: crate::tools::get_tools(&tool_permission_context),
            main_loop_model: None,
            main_loop_profile: None,
            thinking_config: None,
            effort_value: None,
            mcp_state: crate::state::app_state_store::McpState::default(),
            is_non_interactive_session: false,
            agent_id: None,
            agent_type: None,
            require_can_use_tool: false,
            preserve_tool_use_results: false,
            subagent_depth: 0,
            structured_output: None,
            cwd_override: None,
            query_tracking: None,
            tool_permission_context,
            messages: Vec::new(),
            in_progress_tool_use_ids: HashSet::new(),
            set_in_progress_tool_use_ids: SetInProgressToolUseIds::default(),
            set_has_interruptible_tool_in_progress: SetHasInterruptibleToolInProgress::default(),
            interruptible_tool_use_ids: HashSet::new(),
            nested_memory_attachment_triggers: Some(SharedOrderedTriggerSet::fresh()),
            loaded_nested_memory_paths: HashSet::new(),
            dynamic_skill_dir_triggers: Some(SharedOrderedTriggerSet::fresh()),
            discovered_skill_names: HashSet::new(),
            tool_decisions: None,
            tool_use_id: None,
            user_modified: None,
            local_denial_tracking: None,
            file_reading_limits: None,
            glob_limits: None,
            read_file_state: SharedFileStateCache::fresh(),
            bash_tools: Vec::new(),
            content_replacement_state: None,
            rendered_system_prompt: None,
            critical_system_reminder_experimental: None,
            resume_restore_stores: ResumeRestoreStores::default(),
            terminal_notification_requests: Vec::new(),
            terminal_notification_service_plans: Vec::new(),
        }
    }

    /// Attach the live AppStore (CC getAppState/setAppState) and hydrate
    /// permission/mcp snapshots from it.
    pub fn with_app_store(mut self, store: crate::state::store::AppStore) -> Self {
        let snap = store.get();
        self.tool_permission_context = (*snap.tool_permission_context).clone();
        self.mcp_state = (*snap.mcp).clone();
        self.channel_permission_callbacks = snap.channel_permission_callbacks.clone();
        self.verbose = snap.verbose;
        self.agent_definitions = snap.agent_definitions.clone();
        if self.effort_value.is_none() {
            self.effort_value = snap.effort_value.clone();
        }
        if self.main_loop_model.is_none() {
            // Maps to CC wiring `useMainLoopModel()` into
            // `toolUseContext.options.mainLoopModel`: the session profile and
            // its wire model.
            self.main_loop_profile = snap.main_loop_profile.clone();
            self.main_loop_model = Some(crate::utils::model::model::profile_model(
                snap.main_loop_profile.as_deref(),
            ));
        }
        self.app_store = AppStoreRef::new(store);
        self
    }

    /// Attach store handles without overwriting local permission/mcp snapshots
    /// already prepared for a subagent.
    pub fn with_app_store_handles(mut self, handles: AppStoreRef) -> Self {
        self.app_store = handles;
        self
    }

    /// Maps to: CC `ToolUseContext.getAppState()`.
    ///
    /// When `app_store.avoid_permission_prompts_overlay` is set, returns a
    /// cloned view with `shouldAvoidPermissionPrompts` forced on (no store write).
    pub fn get_app_state(&self) -> Option<std::sync::Arc<crate::state::app_state_store::AppState>> {
        let snap = if self.app_store.get_state_override.0.is_some() {
            self.app_store.get_state_override.get()?
        } else {
            self.app_store.store.as_ref()?.get()
        };
        if self.app_store.avoid_permission_prompts_overlay
            && !snap.tool_permission_context.should_avoid_permission_prompts
        {
            let mut state = (*snap).clone();
            let mut permission = (*state.tool_permission_context).clone();
            permission.should_avoid_permission_prompts = true;
            state.tool_permission_context = std::sync::Arc::new(permission);
            return Some(std::sync::Arc::new(state));
        }
        Some(snap)
    }

    /// Maps to: CC `ToolUseContext.setAppState(f)`.
    ///
    /// No-op when `app_store.writable` is false (async subagent default).
    /// B3 flip-audit complete: callers whose CC updater is an unconditional
    /// spread stay on this always-Replace form; callers with a CC
    /// `return prev` guard use [`Self::set_app_state_decided`].
    pub fn set_app_state(&self, f: impl FnOnce(&mut crate::state::app_state_store::AppState)) {
        self.app_store.set_app_state(f);
    }

    /// Guard-carrying variant of [`Self::set_app_state`] for callers whose CC
    /// updater has a `return prev` branch (B3 flip-audit): the closure
    /// observes the current root and returns `Same`/`Replace` explicitly.
    /// Same writable gate as `set_app_state`.
    pub fn set_app_state_decided(
        &self,
        updater: impl FnOnce(
            &crate::state::store::AppStateRoot,
        ) -> crate::state::store::UpdateDecision<()>,
    ) {
        if !self.app_store.writable {
            return;
        }
        if let Some(store) = &self.app_store.store {
            store.set_state(updater);
        }
    }

    /// Maps to: CC `ToolUseContext.setAppStateForTasks`.
    ///
    /// Always reaches the root/tasks store for session-scoped infrastructure,
    /// even when [`Self::set_app_state`] is a no-op.
    pub fn set_app_state_for_tasks(
        &self,
        f: impl FnOnce(&mut crate::state::app_state_store::AppState),
    ) {
        let store = self
            .app_store
            .tasks_store
            .as_ref()
            .or(self.app_store.store.as_ref());
        if let Some(store) = store {
            // B2/B3: same deal as `set_app_state` — always-Replace until the
            // per-caller Same audit at the flip.
            store.replace_with(f);
        }
    }

    /// Maps to: CC `setAppState` for `toolPermissionContext` — updates the
    /// local snapshot and commits to the live store when attached.
    pub fn update_permission_context(&mut self, context: ToolPermissionContext) {
        self.tool_permission_context = context.clone();
        if let Some(store) = &self.app_store.store {
            store.set_tool_permission_context(context);
        }
    }

    /// Push the local permission snapshot to the store (after external mutation).
    pub fn commit_permission_context_to_store(&self) {
        if let Some(store) = &self.app_store.store {
            store.set_tool_permission_context(self.tool_permission_context.clone());
        }
    }

    /// Refresh local permission/mcp snapshots from the live store.
    pub fn refresh_from_app_store(&mut self) {
        let Some(store) = &self.app_store.store else {
            return;
        };
        let snap = store.get();
        self.tool_permission_context = (*snap.tool_permission_context).clone();
        self.mcp_state = (*snap.mcp).clone();
        self.channel_permission_callbacks = snap.channel_permission_callbacks.clone();
        self.verbose = snap.verbose;
        self.agent_definitions = snap.agent_definitions.clone();
    }

    /// Maps to CC reading/writing `ToolUseContext.options` as one object in
    /// `createSubagentContext`.
    pub fn options(&self) -> ToolUseContextOptions {
        ToolUseContextOptions {
            commands: self.commands.clone(),
            debug: self.debug,
            verbose: self.verbose,
            main_loop_model: self.main_loop_model.clone(),
            main_loop_profile: self.main_loop_profile.clone(),
            tools: self.tools.clone(),
            thinking_config: self.thinking_config.clone(),
            mcp_state: self.mcp_state.clone(),
            is_non_interactive_session: self.is_non_interactive_session,
            agent_definitions: self.agent_definitions.clone(),
            max_budget_usd: self.max_budget_usd,
            fallback_model: self.fallback_model.clone(),
            custom_system_prompt: self.custom_system_prompt.clone(),
            append_system_prompt: self.append_system_prompt.clone(),
            query_source: self.query_source_override.clone(),
            refresh_tools: self.refresh_tools.clone(),
        }
    }

    pub fn apply_options(&mut self, options: ToolUseContextOptions) {
        self.commands = options.commands;
        self.debug = options.debug;
        self.verbose = options.verbose;
        self.main_loop_model = options.main_loop_model;
        self.main_loop_profile = options.main_loop_profile;
        self.tools = options.tools;
        self.thinking_config = options.thinking_config;
        self.mcp_state = options.mcp_state;
        self.is_non_interactive_session = options.is_non_interactive_session;
        self.agent_definitions = options.agent_definitions;
        self.max_budget_usd = options.max_budget_usd;
        self.fallback_model = options.fallback_model;
        self.custom_system_prompt = options.custom_system_prompt;
        self.append_system_prompt = options.append_system_prompt;
        self.query_source_override = options.query_source;
        self.refresh_tools = options.refresh_tools;
    }

    pub fn with_get_app_state_override(mut self, callback: GetAppStateCallback) -> Self {
        self.app_store.get_state_override = callback;
        self
    }

    pub fn with_response_length_sink(mut self, sink: ResponseLengthSink) -> Self {
        self.response_length_sink = sink;
        self
    }

    pub fn with_api_metrics_sink(mut self, sink: ApiMetricsSink) -> Self {
        self.api_metrics_sink = sink;
        self
    }

    pub fn with_max_output_tokens_override(mut self, value: Option<u32>) -> Self {
        self.max_output_tokens_override = value;
        self
    }

    pub fn with_agent_definitions(
        mut self,
        definitions: std::sync::Arc<
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionsResult,
        >,
    ) -> Self {
        self.agent_definitions = definitions;
        self
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    /// What a request made from this session inherits (`inherit`, `@tier` with
    /// nothing to name): its current model, effort and profile.
    pub fn parent_selection(&self) -> crate::utils::model::agent::ParentSelection {
        crate::utils::model::agent::ParentSelection {
            model: self
                .main_loop_model
                .clone()
                .unwrap_or_else(crate::utils::model::model::get_main_loop_model),
            effort: self.effort_value,
            profile: self.main_loop_profile.clone(),
        }
    }

    /// Name of the profile this session's requests go through: the one it was
    /// switched to, else `selection.main`.
    pub fn effective_profile_name(&self) -> Option<String> {
        self.main_loop_profile.clone().or_else(|| {
            crate::utils::settings::get_initial_settings()
                .nyan
                .as_deref()
                .and_then(|nyan| nyan.selection.as_ref())
                .and_then(|selection| selection.main.clone())
        })
    }

    pub fn with_main_loop_model(mut self, main_loop_model: impl Into<String>) -> Self {
        self.main_loop_model = Some(main_loop_model.into());
        self
    }

    pub fn with_thinking_config(
        mut self,
        thinking_config: Option<crate::utils::thinking::ThinkingConfig>,
    ) -> Self {
        self.thinking_config = thinking_config;
        self
    }

    pub fn with_effort_value(mut self, effort_value: Option<crate::utils::effort::Effort>) -> Self {
        self.effort_value = effort_value;
        self
    }

    pub fn with_mcp_state(mut self, mcp_state: crate::state::app_state_store::McpState) -> Self {
        self.mcp_state = mcp_state;
        self
    }

    pub fn with_channel_permission_callbacks(
        mut self,
        callbacks: Option<crate::services::mcp::channel_permissions::ChannelPermissionCallbacks>,
    ) -> Self {
        self.channel_permission_callbacks = callbacks;
        self
    }

    pub fn with_non_interactive_session(mut self, is_non_interactive_session: bool) -> Self {
        self.is_non_interactive_session = is_non_interactive_session;
        self
    }

    pub fn with_agent_id(mut self, agent_id: Option<String>) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn with_cwd_override(mut self, cwd_override: Option<std::path::PathBuf>) -> Self {
        self.cwd_override = cwd_override;
        self
    }

    /// Maps to CC `utils/cwd.ts#getCwd` for tool code that needs a concrete
    /// filesystem root while preserving process-global cwd isolation.
    pub fn effective_cwd(&self) -> std::path::PathBuf {
        self.cwd_override
            .clone()
            .unwrap_or_else(crate::bootstrap::state::get_original_cwd)
    }

    pub fn with_query_tracking(mut self, query_tracking: Option<QueryChainTracking>) -> Self {
        self.query_tracking = query_tracking;
        self
    }

    pub fn with_resume_payloads(
        mut self,
        read_file_state: Vec<ReadFileStateEntry>,
        bash_tools: Vec<String>,
    ) -> Self {
        // A resume/new top-level context gets a detached cache identity; it
        // must not replace the contents of a cache shared by another live turn.
        self.read_file_state = SharedFileStateCache::from_entries(read_file_state.clone());
        self.bash_tools = bash_tools.clone();
        self.resume_restore_stores.read_file_state = read_file_state;
        self.resume_restore_stores.bash_tools = bash_tools;
        self
    }

    pub fn with_content_replacement_state(
        mut self,
        content_replacement_state: Option<
            crate::utils::tool_result_storage::ContentReplacementState,
        >,
    ) -> Self {
        self.content_replacement_state = content_replacement_state;
        self
    }

    pub fn with_resume_restore_stores(mut self, stores: ResumeRestoreStores) -> Self {
        self.read_file_state = SharedFileStateCache::from_entries(stores.read_file_state.clone());
        self.bash_tools = stores.bash_tools.clone();
        self.resume_restore_stores = stores;
        self
    }

    /// Explicitly retain all three independently shared Read-visible handles
    /// when a Rust actor adopts a whole context snapshot while workers run.
    pub(crate) fn retain_file_read_handles_from(&mut self, live: &ToolUseContext) {
        self.read_file_state = live.read_file_state.clone();
        self.nested_memory_attachment_triggers = live.nested_memory_attachment_triggers.clone();
        self.dynamic_skill_dir_triggers = live.dynamic_skill_dir_triggers.clone();
    }

    pub fn with_critical_system_reminder_experimental(mut self, reminder: Option<String>) -> Self {
        self.critical_system_reminder_experimental = reminder;
        self
    }

    /// Maps to: CC tool execution calling
    /// `setInProgressToolUseIDs(prev => new Set(prev).add(id))` when a tool
    /// starts. The local mirror and the REPL-owned set move together.
    pub fn mark_in_progress(&mut self, tool_use_id: impl Into<String>) {
        self.mark_in_progress_with_behavior(tool_use_id, InterruptBehavior::Block);
    }

    /// Maps to: CC `StreamingToolExecutor.executeTool` adding a tracked tool
    /// and recomputing `hasInterruptibleToolInProgress` from all executing
    /// tools.  The default `mark_in_progress` remains the block behavior for
    /// legacy callers that do not have a tool definition at the call site.
    pub fn mark_in_progress_with_behavior(
        &mut self,
        tool_use_id: impl Into<String>,
        behavior: InterruptBehavior,
    ) {
        let tool_use_id = tool_use_id.into();
        if let Some(set) = self.set_in_progress_tool_use_ids.0.as_ref() {
            set(tool_use_id.as_str(), true);
        }
        self.in_progress_tool_use_ids.insert(tool_use_id.clone());
        if behavior == InterruptBehavior::Cancel {
            self.interruptible_tool_use_ids.insert(tool_use_id.clone());
        } else {
            self.interruptible_tool_use_ids.remove(&tool_use_id);
        }
        self.update_interruptible_tool_state();
    }

    /// Maps to: CC tool execution calling
    /// `setInProgressToolUseIDs(prev => { const next = new Set(prev); next.delete(id); return next })`
    /// when a tool finishes, whether it resolved or errored.
    pub fn mark_complete(&mut self, tool_use_id: &str) {
        if let Some(set) = self.set_in_progress_tool_use_ids.0.as_ref() {
            set(tool_use_id, false);
        }
        self.in_progress_tool_use_ids.remove(tool_use_id);
        self.interruptible_tool_use_ids.remove(tool_use_id);
        self.update_interruptible_tool_state();
    }

    fn update_interruptible_tool_state(&self) {
        let value = !self.in_progress_tool_use_ids.is_empty()
            && self.in_progress_tool_use_ids.len() == self.interruptible_tool_use_ids.len();
        if let Some(set) = self.set_has_interruptible_tool_in_progress.0.as_ref() {
            set(value);
        }
    }

    pub fn record_terminal_notification_request(&mut self, request: TerminalNotificationRequest) {
        self.terminal_notification_requests.push(request);
    }

    pub fn record_terminal_notification_service_plan(
        &mut self,
        plan: TerminalNotificationServicePlan,
    ) {
        self.terminal_notification_requests
            .push(plan.request.clone());
        self.terminal_notification_service_plans.push(plan);
    }

    pub fn record_terminal_notification_from_config(
        &mut self,
        options: TerminalNotificationOptions,
        config: &GlobalConfig,
        env: &TerminalNotificationEnvironment<'_>,
    ) {
        let plan = terminal_notification_service_plan_from_config(options, config, env);
        self.record_terminal_notification_service_plan(plan);
    }
}

// ─── Tool behavior interface ──────────────────────────────────────────────

/// Result of a tool execution.
/// Maps to: CC `Tool.ts` `ToolResult<T>` (:321) —
/// `{ data, newMessages?, contextModifier?, mcpMeta? }`.
///
/// `contextModifier` is carried by the orchestration layer's
/// `services/tools/tool_execution.rs#ToolContextModifier` (its
/// `ToolContextModifierOperation` enum is the typed stand-in for CC's
/// `(context: ToolUseContext) => ToolUseContext` closure). It is NOT a field
/// here: `SkillTool` is the only producer in the tree (`SkillTool.ts:775-839`),
/// and adding a required field to this struct would rewrite every tool's
/// construction site. The producer therefore hands the operation out on its own
/// typed output — `tools/skill_tool/mod.rs#Output::Inline.context_modifier` —
/// and `check_permissions_and_call_tool` reads it at CC's read position
/// (`toolExecution.ts:1400`) into `LocalToolExecutionMessageResult`, pairing it
/// with the tool_use id (`:1465-1470`) on `ToolExecutionResult`. Flipping this
/// to a real field is mechanical once `ToolResult` gains a builder.
///
/// `mcpMeta` is not yet ported.
pub struct ToolResult {
    /// CC `ToolResult.data` — the tool's typed output.
    pub data: ToolOutput,
    /// CC `ToolResult.newMessages` (Tool.ts:323). Preserved by tool
    /// execution and yielded after the primary tool_result; most ports still
    /// return an empty list until their CC side effects are migrated.
    pub new_messages: Vec<crate::types::message::Message>,
}

/// Per-tool output carried in `ToolResult.data`.
/// Maps to: CC per-tool `Tool.outputSchema` types. Tools migrate variant by
/// variant from the transitional `Composed` shape to a real CC-shaped output;
/// each new variant must be cross-reviewed against that tool's CC
/// `outputSchema` and `mapToolResultToToolResultBlockParam`.
pub enum ToolOutput {
    /// Pre-composed result row for non-schema paths: tool execution errors
    /// (CC throws and wraps via `classifyToolError`; this port returns the
    /// composed error row directly) and unported-tool stubs. Every success
    /// path now flows through a typed per-tool variant below.
    Composed {
        content: String,
        status: crate::types::message::ToolResultStatus,
    },
    /// L1 (`FileReadTool fallible source-position state carrier`): the exact
    /// Read-only call envelope approved for the unchanged generic trait bridge.
    /// Generic execution projects it once and never reconstructs Read state or
    /// switches on Read modalities.
    FileReadCall(crate::tools::file_read_tool::FileReadRegistryOutcome),
    /// CC `tools/FileWriteTool/FileWriteTool.ts` outputSchema (:68).
    Write(crate::tools::file_write_tool::WriteOutput),
    /// Rust transport for a post-discovery Write failure. CC throws rather
    /// than producing output-schema data, but its per-context dynamic skill
    /// trigger Set has already been mutated; this carrier preserves that
    /// context effect without presenting a successful typed Write result.
    WriteError(crate::tools::file_write_tool::WriteErrorOutput),
    /// CC `tools/FileEditTool/types.ts` outputSchema (:63).
    Edit(crate::tools::file_edit_tool::EditOutput),
    /// Rust carrier for a post-discovery Edit failure; see `WriteError`.
    EditError(crate::tools::file_edit_tool::EditErrorOutput),
    /// CC `tools/GlobTool/GlobTool.ts` outputSchema (:39).
    Glob(crate::tools::glob_tool::Output),
    /// CC `tools/GrepTool/GrepTool.ts` outputSchema (:144).
    Grep(crate::tools::grep_tool::Output),
    /// @nyan: structured CodeSearch result; model-facing text remains in the
    /// normal tool-result content while the raw carrier preserves counts.
    CodeSearch(crate::tools::code_search_tool::Output),
    /// CC `tools/BashTool/BashTool.tsx` outputSchema (:439).
    Bash(crate::tools::bash_tool::BashOutput),
    /// CC `tools/PowerShellTool/PowerShellTool.tsx` outputSchema (:310).
    PowerShell(crate::tools::powershell_tool::PowerShellOutput),
    /// CC `tools/TodoWriteTool/TodoWriteTool.ts` outputSchema (:20).
    TodoWrite(crate::tools::todo_write_tool::TodoWriteOutput),
    /// CC `tools/AskUserQuestionTool/AskUserQuestionTool.tsx` outputSchema
    /// (:128).
    AskUserQuestion(crate::tools::ask_user_question_tool::Output),
    /// CC `tools/LSPTool/LSPTool.ts` outputSchema (:76).
    Lsp(crate::tools::lsp_tool::Output),
    /// CC `tools/WebFetchTool/WebFetchTool.ts` outputSchema (:32).
    WebFetch(crate::tools::web_fetch_tool::Output),
    /// CC `tools/WebSearchTool/WebSearchTool.ts` outputSchema (:56).
    WebSearch(crate::tools::web_search_tool::Output),
    /// CC `tools/SkillTool/SkillTool.ts` outputSchema (:301).
    Skill(crate::tools::skill_tool::Output),
    /// CC `tools/AgentTool/AgentTool.tsx` outputSchema (:271).
    Agent(crate::tools::agent_tool::AgentOutput),
    /// CC `tools/EnterPlanModeTool/EnterPlanModeTool.ts` outputSchema (:23).
    EnterPlanMode(crate::tools::enter_plan_mode_tool::Output),
    /// CC `tools/ExitPlanModeTool/ExitPlanModeV2Tool.ts` outputSchema (:84).
    ExitPlanMode(crate::tools::exit_plan_mode_tool::Output),
    /// CC `tools/TaskCreateTool/TaskCreateTool.ts` outputSchema (:22).
    TaskCreate(crate::tools::task_create_tool::TaskCreateOutput),
    /// CC `tools/TaskGetTool/TaskGetTool.ts` outputSchema (:20).
    TaskGet(crate::tools::task_get_tool::TaskGetOutput),
    /// CC `tools/TaskListTool/TaskListTool.ts` outputSchema (:18).
    TaskList(crate::tools::task_list_tool::TaskListOutput),
    /// CC `tools/TaskUpdateTool/TaskUpdateTool.ts` outputSchema (:60).
    TaskUpdate(crate::tools::task_update_tool::TaskUpdateOutput),
    /// CC `tools/BriefTool/BriefTool.ts` outputSchema (:42).
    Brief(crate::tools::brief_tool::BriefOutput),
    /// CC `tools/SyntheticOutputTool/SyntheticOutputTool.ts` output.
    SyntheticOutput(crate::tools::synthetic_output_tool::SyntheticOutput),
    /// CC `tools/SendMessageTool/SendMessageTool.ts` output union (:127).
    SendMessage(crate::tools::send_message_tool::SendMessageOutput),
    /// CC `tools/TeamCreateTool/TeamCreateTool.ts` output type (:48).
    TeamCreate(crate::tools::team_create_tool::TeamCreateOutput),
    /// CC `tools/TeamDeleteTool/TeamDeleteTool.ts` outputSchema (:30).
    TeamDelete(crate::tools::team_delete_tool::TeamDeleteOutput),
    /// CC `tools/ScheduleCronTool/CronCreateTool.ts` outputSchema (:45).
    CronCreate(crate::tools::schedule_cron_tool::CreateOutput),
    /// CC `tools/ScheduleCronTool/CronDeleteTool.ts` outputSchema (:27).
    CronDelete(crate::tools::schedule_cron_tool::DeleteOutput),
    /// CC `tools/ScheduleCronTool/CronListTool.ts` outputSchema (:20).
    CronList(crate::tools::schedule_cron_tool::ListOutput),
    /// CC `tools/EnterWorktreeTool/EnterWorktreeTool.ts` outputSchema (:42).
    EnterWorktree(crate::tools::enter_worktree_tool::Output),
    /// CC `tools/ExitWorktreeTool/ExitWorktreeTool.ts` outputSchema (:47).
    ExitWorktree(crate::tools::exit_worktree_tool::Output),
    /// CC `tools/ToolSearchTool/ToolSearchTool.ts` outputSchema (:37).
    ToolSearch(crate::tools::tool_search_tool::ToolSearchOutput),
    /// CC `tools/TaskStopTool/TaskStopTool.ts` outputSchema (:22).
    TaskStop(crate::tools::task_stop_tool::Output),
    /// CC `tools/TaskOutputTool/TaskOutputTool.tsx` output type (:62).
    TaskOutput(crate::tools::task_output_tool::Output),
    /// CC `tools/NotebookEditTool/NotebookEditTool.ts` outputSchema (:60).
    NotebookEdit(crate::tools::notebook_edit_tool::Output),
    /// CC `tools/ListMcpResourcesTool/ListMcpResourcesTool.ts` outputSchema (:22).
    ListMcpResources(crate::tools::list_mcp_resources_tool::Output),
    /// CC `tools/ReadMcpResourceTool/ReadMcpResourceTool.ts` outputSchema (:22).
    ReadMcpResource(crate::tools::read_mcp_resource_tool::Output),
    /// CC `tools/MCPTool/MCPTool.ts` outputSchema (:16).
    Mcp(crate::tools::mcp_tool::McpOutput),
}

// Mirrors CC `Tool.ts:12` `import type { CanUseToolFn } from
// './hooks/useCanUseTool.js'` — the type is owned by the useCanUseTool
// counterpart; this re-export keeps `Tool.call` signatures referencing it
// through the Tool boundary, as CC does.
pub use crate::hooks::use_can_use_tool::CanUseToolFn;

/// Streaming progress callback.
/// Maps to: CC `Tool.ts` `ToolCallProgress<P>` (:338).
pub type ToolCallProgressFn<'a> = &'a (dyn Fn(crate::types::tools::ToolProgress) + Send + Sync);

/// Maps to: CC `Tool.ts` `ValidationResult` (:95-101).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    Ok,
    Error {
        message: String,
        error_code: i32,
    },
    /// Validation itself threw/rejected in CC and must flow through the outer
    /// `Error calling tool (<name>): ...` boundary rather than semantic error
    /// telemetry/code handling.
    Fatal {
        message: String,
    },
}

impl ValidationResult {
    pub fn ok() -> Self {
        Self::Ok
    }

    pub fn error(message: impl Into<String>, error_code: i32) -> Self {
        Self::Error {
            message: message.into(),
            error_code,
        }
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self::Fatal {
            message: message.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// What happens to a running tool when the user submits a new message.
/// Maps to: CC `Tool.ts` `interruptBehavior?(): 'cancel' | 'block'` (:416),
/// consumed by `StreamingToolExecutor.ts:233-241`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptBehavior {
    /// Stop the tool and discard its result.
    Cancel,
    /// Keep running; the new message waits.
    Block,
}

/// Default max result size before tool-result persistence (CC tools that do not
/// set Infinity use 100_000). Maps to CC `Tool.maxResultSizeChars` default usage.
pub const DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 100_000;

/// Sentinel for CC `maxResultSizeChars: Infinity` (e.g. Read) — skip result
/// budget truncation / disk persistence for this tool.
pub const UNBOUNDED_MAX_RESULT_SIZE_CHARS: usize = usize::MAX;

/// Prepared matcher for hook `if` conditions such as `Bash(git *)`.
/// Maps to CC `Tool.preparePermissionMatcher`.
pub type PermissionPatternMatcher = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Maps to: CC `Tool.ts:518-523` — the options object of the REQUIRED
/// `Tool.prompt(options)` interface member, the API-facing tool description
/// rendered lazily at serialization time (`utils/api.ts:169-176`).
///
/// CC field ↔ Rust field:
/// - `getToolPermissionContext: () => Promise<ToolPermissionContext>` →
///   `tool_permission_context`. CC's member is an async factory only because
///   its producer is an async `getAppState()` read (`query.ts:666-669`). Every
///   Rust producer at this boundary is synchronous — the query actor already
///   holds the live projection as a plain value
///   (`query.rs#build_call_model_request` `permission_context`), and the
///   serialization site `build_sdk_message_create_plan` is itself a sync `fn`
///   — so per PORTING.md § Node 异步模型 (the A4 sync-interface leg: no
///   genuinely-async producer exists) the DERIVED faithful shape is a sync
///   borrow of the already-resolved value. Evaluation position is preserved:
///   CC resolves the factory inside the schema-cache miss branch during
///   request assembly; the Rust snapshot is taken in the same request
///   assembly with no await between snapshot and use on the actor path.
/// - `tools: Tools` → `tools` (the FULL request tool list, not the
///   defer-filtered one — `services/api/claude.ts:1231-1239`).
/// - `agents: AgentDefinition[]` → `agents`.
#[derive(Clone, Copy)]
pub struct ToolPromptOptions<'a> {
    pub tool_permission_context: &'a ToolPermissionContext,
    pub tools: &'a [crate::types::tools::Tool],
    pub agents: &'a [crate::tools::agent_tool::load_agents_dir::AgentDefinition],
}

/// Behavioral half of the CC `Tool` interface.
/// Maps to: CC `Tool.ts` `Tool` (:362) — `call` (:379),
/// `mapToolResultToToolResultBlockParam` (:557), `renderToolResultMessage`
/// (:566, a React component in CC; a display-payload builder in this port).
///
/// Defaultable methods mirror CC `buildTool` / `TOOL_DEFAULTS` (Tool.ts:748-769):
/// fail-closed for concurrency/read-only; `check_permissions` defers to the
/// general permission system with allow+updatedInput.
pub trait ToolCall: Sync {
    /// Primary tool name (CC `Tool.name`).
    fn name(&self) -> &'static str;

    /// Lookup aliases (CC `Tool.aliases`, Tool.ts:371).
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Maps to: CC `Tool.ts:378-394`, where behavior methods receive
    /// `z.infer<Input>` from the Tool's `inputSchema`. Rust stores API JSON
    /// Schema separately, so this L1 adapter carries only the schema's parsed
    /// data projection; each invocation owner performs its own parse step.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        args.clone()
    }

    /// Context-aware projection of CC `utils/api.ts#normalizeToolInput`.
    /// Most schemas only need the context-free semantic parser above; Edit
    /// additionally reads its cwd-scoped file for de-sanitization.
    fn normalize_input_with_context(
        &self,
        args: &serde_json::Value,
        _context: &ToolUseContext,
    ) -> serde_json::Value {
        self.normalize_input(args)
    }

    /// Maps to optional CC `Tool.inputsEquivalent(a, b)`. `None` means the
    /// tool has no semantic comparator and interactive updates are not marked
    /// as user-modified.
    fn inputs_equivalent(
        &self,
        _left: &serde_json::Value,
        _right: &serde_json::Value,
        _context: &ToolUseContext,
    ) -> Result<Option<bool>, String> {
        Ok(None)
    }

    /// Maps to: CC `Tool.backfillObservableInput(input, context)`.
    ///
    /// This is deliberately separate from `normalize_input`: backfilled input
    /// is observed by validation, hooks, permissions, and UI while CC normally
    /// calls the tool body with the original model input. The execution owner
    /// carries that distinction through its `callInput` seam.
    fn backfill_observable_input(
        &self,
        args: &serde_json::Value,
        _context: &ToolUseContext,
    ) -> serde_json::Value {
        args.clone()
    }

    /// Maps to: CC `Tool.isEnabled()` (default true).
    fn is_enabled(&self) -> bool {
        true
    }

    /// Maps to: CC `Tool.ts` `isConcurrencySafe(input)` (default false).
    /// `runTools(...)` uses this per-tool seam when building concurrent
    /// read-only batches instead of owning tool-specific knowledge itself.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Maps to CC `Tool.isSearchOrReadCommand(...)`.
    fn is_search_or_read_command(&self, _args: &serde_json::Value) -> Option<SearchOrReadCommand> {
        None
    }

    /// Maps to: CC `Tool.isReadOnly(input)` (default false — assume writes).
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Maps to: CC `Tool.isDestructive?(input)` (default false).
    fn is_destructive(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Maps to: CC `Tool.interruptBehavior?()` (Tool.ts:416). CC treats an
    /// unimplemented method — and a throwing one — as `'block'`
    /// (`StreamingToolExecutor.ts:233-241`).
    fn interrupt_behavior(&self) -> InterruptBehavior {
        InterruptBehavior::Block
    }

    /// Maps to: CC `Tool.isOpenWorld?(input)` (Tool.ts:434). Every consumer
    /// reads it as `tool.isOpenWorld?.({}) ?? false` (`cli/print.ts:1662`,
    /// `components/mcp/MCPToolListView.tsx:46`); MCP tools source it from
    /// `annotations.openWorldHint` (`services/mcp/client.ts:1807-1809`).
    fn is_open_world(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Maps to: CC `Tool.requiresUserInteraction?()` (default false).
    /// When true, bypassPermissions must still prompt (CC permissions.ts 1e / 1f).
    fn requires_user_interaction(&self) -> bool {
        false
    }

    /// Maps to: CC `Tool.searchHint` — keyword string for ToolSearch.
    fn search_hint(&self) -> Option<&'static str> {
        None
    }

    /// Maps to: CC `Tool.userFacingName(input)` (Tool.ts:524). `buildTool`
    /// fills the default with `() => def.name` (Tool.ts:789), which overrides
    /// the `''` in `TOOL_DEFAULTS`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        self.name().to_string()
    }

    /// Maps to: CC `Tool.isTransparentWrapper?()` (Tool.ts:533), read as
    /// `tool.isTransparentWrapper?.() ?? false`
    /// (`AssistantToolUseMessage.tsx:96`). A wrapper delegates all rendering to
    /// its progress handler and shows no chrome of its own.
    fn is_transparent_wrapper(&self) -> bool {
        false
    }

    /// Short summary of this tool use for compact views.
    /// Maps to: CC `Tool.getToolUseSummary?(input)` (Tool.ts:539), an
    /// optional interface hook with three cross-tool consumers:
    /// `toolHooks.ts:475` (pre-tool hook payload), `structuredIO.ts:105`
    /// (requires_action description fallback chain), and
    /// `AgentTool/UI.tsx:1110` (collapsed subagent progress).
    fn get_tool_use_summary(&self, _args: &serde_json::Value) -> Option<String> {
        None
    }

    /// Maps to CC `Tool.getActivityDescription?(input)`.
    fn get_activity_description(&self, _args: &serde_json::Value) -> Option<String> {
        None
    }

    /// Maps to: CC `Tool.prompt(options)` (Tool.ts:518-523) — the REQUIRED
    /// model/API-facing tool description, rendered lazily per request and
    /// consumed only at serialization sites (`utils/api.ts:171`,
    /// `utils/toolSearch.ts:350`, `utils/analyzeContext.ts:652`,
    /// `entrypoints/mcp.ts:85`; quoted census:
    /// `rg -n "\.prompt\(" ../rebuild/src` outside `tools/`).
    ///
    /// `tool` is CC's `this`: CC `prompt` is an instance method on the tool
    /// object, and this port splits CC's tool object into wire metadata
    /// (`types::tools::Tool`, the instance) plus this process-wide behavior
    /// singleton. Instance-bound prompts must read the instance:
    /// - MCP tools return their (truncated) server-provided description
    ///   (`services/mcp/client.ts:1789-1794`), stored in `Tool.description`;
    /// - `StructuredOutput` instances carry a per-instance override
    ///   (`utils/hooks/hookHelpers.ts:60-63` spreads the base tool and
    ///   replaces `prompt()`), also stored in `Tool.description`.
    /// Constant-return tools (the other ~37, e.g. `GlobTool.ts:143`) ignore
    /// both parameters and forward to the same source their schema
    /// constructor renders eagerly.
    ///
    /// Async ruling: sync `fn` — see [`ToolPromptOptions`] for the tier
    /// evidence (every producer of every option value resolves synchronously
    /// at this boundary).
    fn prompt(&self, tool: &crate::types::tools::Tool, options: &ToolPromptOptions<'_>) -> String;

    /// Short, input-derived line the permission dialog shows for this tool use
    /// — distinct from `prompt()`, which is the model-facing tool description
    /// this port stores in `types::tools::Tool::description`.
    /// Maps to: CC `Tool.description(input, options)` (Tool.ts:386-393), read
    /// at `useCanUseTool.tsx:138` and `swarm/inProcessRunner.ts:185`.
    ///
    /// `TOOL_DEFAULTS` (Tool.ts:757-769) has no entry for it: `ToolDef` makes
    /// it required, so CC has no fallback and an owner that has not ported its
    /// copy contributes nothing. Only Bash, WebSearch and WebFetch read the
    /// input; the other 32 return a constant, and none reads the `options` bag.
    fn description(&self, _args: &serde_json::Value) -> String {
        String::new()
    }

    /// Maps to: CC `Tool.maxResultSizeChars`. Use
    /// [`UNBOUNDED_MAX_RESULT_SIZE_CHARS`] for Infinity (Read).
    fn max_result_size_chars(&self) -> usize {
        DEFAULT_MAX_RESULT_SIZE_CHARS
    }

    /// Maps to: CC `Tool.shouldDefer` (default false).
    fn should_defer(&self) -> bool {
        false
    }

    /// Maps to: CC `Tool.validateInput?(input, context)`.
    /// Default: always valid (schema validation runs earlier in tool_execution).
    fn validate_input(
        &self,
        _args: &serde_json::Value,
        _context: &ToolUseContext,
    ) -> ValidationResult {
        ValidationResult::Ok
    }

    /// Maps to: CC `Tool.checkPermissions(input, context)` (default allow +
    /// updatedInput — defer to general permission system / TOOL_DEFAULTS).
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        _context: &ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        crate::utils::permissions::permission_result::PermissionResult::Allow {
            updated_input: Some(args.clone()),
            user_modified: None,
            decision_reason: None,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }

    /// Maps to: CC `Tool.getPath?(input)` — file path tools override.
    fn get_path(&self, _args: &serde_json::Value) -> Option<String> {
        None
    }

    /// Maps to: CC `Tool.toAutoClassifierInput(input)` (default '' = skip).
    fn to_auto_classifier_input(&self, _args: &serde_json::Value) -> String {
        String::new()
    }

    /// Maps to CC `Tool.preparePermissionMatcher?(input)`. The returned
    /// closure is prepared once per hook event and then evaluated for each
    /// configured hook `if` condition.
    fn prepare_permission_matcher(
        &self,
        _args: &serde_json::Value,
    ) -> Option<PermissionPatternMatcher> {
        None
    }

    /// Execute a permitted tool use.
    /// Maps to: CC `Tool.call(args, context, canUseTool, parentMessage,
    /// onProgress)` (Tool.ts:379-385).
    /// `request` is a Rust seam carrying tool_use_id and the raw permission
    /// payload (CC threads `toolUseId` through `context`).
    /// TODO(parity): several ports still re-parse `request` instead of
    /// consuming `args`, and most ports do not yet consume
    /// `can_use_tool`/`parent_message`/`on_progress`.
    /// Async seam: CC `call` is async and can stream progress; this port
    /// returns a boxed future (tool bodies are still synchronous — real
    /// streaming lands with the StreamingToolExecutor port).
    #[allow(clippy::too_many_arguments)]
    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a ToolUseContext,
        can_use_tool: Option<CanUseToolFn<'a>>,
        parent_message: Option<&'a crate::types::message::AssistantMessage>,
        on_progress: Option<ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, ToolResult>;

    /// Map the tool output to the model-visible `tool_result` block content.
    /// Maps to: CC `Tool.mapToolResultToToolResultBlockParam(content,
    /// toolUseID)` (Tool.ts:557). Returns (content, status) — status carries
    /// CC `ToolResultBlockParam.is_error`.
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            other => (
                format!(
                    "<tool_use_error>tool output variant not handled by this \
                     tool's result mapper: {}</tool_use_error>",
                    output_variant_name(other)
                ),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// The raw per-tool output CC records as `toolUseResult` on the user
    /// message (`toolExecution.ts` attaches the tool's typed output object
    /// verbatim; render consumes it via `outputSchema.safeParse`).
    ///
    /// `None` means the tool records no output at all. CC always records
    /// something, so a tool whose success rows should render must return
    /// `Some` — the render layer treats a missing raw as "render nothing"
    /// (`UserToolSuccessMessage.tsx:72`).
    fn tool_use_result(&self, _data: &ToolOutput) -> Option<serde_json::Value> {
        None
    }

    /// Flattened text of what the transcript-mode result render shows, for the
    /// transcript search index.
    /// Maps to: CC `Tool.extractSearchText?(out)` (Tool.ts:599). The optionality
    /// is load-bearing at the consumer (`Messages.tsx:896-901`): `None` is "tool
    /// didn't implement it" and keeps the `renderableSearchText` field-name
    /// heuristic, while `Some("")` is the tool declaring it has nothing to index
    /// (`FileReadTool.ts:414-416`, `WebSearchTool.ts:229-234`).
    fn extract_search_text(&self, _data: &ToolOutput) -> Option<String> {
        None
    }

    /// Whether the non-verbose render of this output is truncated, gating
    /// fullscreen click-to-expand.
    /// Maps to: CC `Tool.isResultTruncated?(output)` (Tool.ts:615), read as
    /// `tool?.isResultTruncated?.(...) ?? false` (`Messages.tsx:759`).
    fn is_result_truncated(&self, _data: &ToolOutput) -> bool {
        false
    }
}

fn output_variant_name(output: &ToolOutput) -> &'static str {
    match output {
        ToolOutput::Composed { .. } => "Composed",
        ToolOutput::FileReadCall(_) => "FileReadCall",
        ToolOutput::Write(_) => "Write",
        ToolOutput::WriteError(_) => "WriteError",
        ToolOutput::Edit(_) => "Edit",
        ToolOutput::EditError(_) => "EditError",
        ToolOutput::Glob(_) => "Glob",
        ToolOutput::Grep(_) => "Grep",
        ToolOutput::CodeSearch(_) => "CodeSearch",
        ToolOutput::Bash(_) => "Bash",
        ToolOutput::PowerShell(_) => "PowerShell",
        ToolOutput::TodoWrite(_) => "TodoWrite",
        ToolOutput::AskUserQuestion(_) => "AskUserQuestion",
        ToolOutput::Lsp(_) => "Lsp",
        ToolOutput::WebFetch(_) => "WebFetch",
        ToolOutput::WebSearch(_) => "WebSearch",
        ToolOutput::Skill(_) => "Skill",
        ToolOutput::Agent(_) => "Agent",
        ToolOutput::EnterPlanMode(_) => "EnterPlanMode",
        ToolOutput::ExitPlanMode(_) => "ExitPlanMode",
        ToolOutput::TaskCreate(_) => "TaskCreate",
        ToolOutput::TaskGet(_) => "TaskGet",
        ToolOutput::TaskList(_) => "TaskList",
        ToolOutput::TaskUpdate(_) => "TaskUpdate",
        ToolOutput::Brief(_) => "Brief",
        ToolOutput::SendMessage(_) => "SendMessage",
        ToolOutput::TeamCreate(_) => "TeamCreate",
        ToolOutput::TeamDelete(_) => "TeamDelete",
        ToolOutput::CronCreate(_) => "CronCreate",
        ToolOutput::CronDelete(_) => "CronDelete",
        ToolOutput::CronList(_) => "CronList",
        ToolOutput::EnterWorktree(_) => "EnterWorktree",
        ToolOutput::ExitWorktree(_) => "ExitWorktree",
        ToolOutput::ToolSearch(_) => "ToolSearch",
        ToolOutput::TaskStop(_) => "TaskStop",
        ToolOutput::TaskOutput(_) => "TaskOutput",
        ToolOutput::NotebookEdit(_) => "NotebookEdit",
        ToolOutput::ListMcpResources(_) => "ListMcpResources",
        ToolOutput::ReadMcpResource(_) => "ReadMcpResource",
        ToolOutput::Mcp(_) => "Mcp",
        ToolOutput::SyntheticOutput(_) => "SyntheticOutput",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::query_helpers::{ReadFileStateEntry, ReadFileStateSource};
    use crate::utils::session_restore::{RestoredContextCollapseState, RestoredFileHistoryState};
    use crate::utils::terminal_notification::{
        TerminalNotificationChannel, TerminalNotificationMethod, TerminalNotificationOptions,
        TerminalNotificationRequest,
    };

    #[test]
    fn source_keyed_permission_context_serde_round_trips_all_sources() {
        use crate::types::permissions::{PermissionRuleSource, PermissionRuleValue};

        let mut context = ToolPermissionContext::default();
        for source in [
            PermissionRuleSource::UserSettings,
            PermissionRuleSource::ProjectSettings,
            PermissionRuleSource::LocalSettings,
            PermissionRuleSource::FlagSettings,
            PermissionRuleSource::CliArg,
            PermissionRuleSource::Command,
            PermissionRuleSource::Session,
        ] {
            context.always_allow_rules.insert(
                source,
                vec![PermissionRuleValue::new(format!("Tool{source:?}"), None)],
            );
        }
        context.stripped_dangerous_rules = Some(context.always_allow_rules.clone());

        let value = serde_json::to_value(&context).unwrap();
        let allow = value["always_allow_rules"].as_object().unwrap();
        for key in [
            "userSettings",
            "projectSettings",
            "localSettings",
            "flagSettings",
            "cliArg",
            "command",
            "session",
        ] {
            assert!(allow.contains_key(key), "missing source key {key}");
            assert!(value["stripped_dangerous_rules"].get(key).is_some());
        }
        let restored: ToolPermissionContext = serde_json::from_value(value).unwrap();
        assert_eq!(restored, context);
    }

    #[test]
    fn abort_controller_child_observes_parent_abort() {
        let parent = AbortController::default();
        let child = AbortController::child_of(parent.clone());

        assert!(!child.is_aborted());
        parent.abort();
        assert!(child.is_aborted());
    }

    #[test]
    fn abort_controller_sdk_signal_aborts_with_controller() {
        let controller = AbortController::default();
        let signal = controller.signal();
        assert!(!signal.is_aborted());
        controller.abort();
        assert!(signal.is_aborted());
        assert!(controller.is_aborted());
    }

    #[test]
    fn abort_controller_sdk_signal_is_cached_per_controller() {
        let controller = AbortController::default();
        let a = controller.signal();
        let b = controller.signal();
        // Same underlying watch channel: abort one view, both observe.
        assert!(!a.is_aborted());
        assert!(!b.is_aborted());
        controller.abort();
        assert!(a.is_aborted());
        assert!(b.is_aborted());
    }

    #[test]
    fn abort_controller_parent_abort_cancels_child_sdk_signal() {
        let parent = AbortController::default();
        let child = AbortController::child_of(parent.clone());
        let signal = child.signal();
        assert!(!signal.is_aborted());
        parent.abort();
        assert!(signal.is_aborted());
        assert!(child.is_aborted());
    }

    #[test]
    fn tool_use_context_carries_messages_for_query_view() {
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "messagesForQuery".to_string(),
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

        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_messages(messages.clone());

        assert_eq!(context.messages, messages);
        assert!(crate::utils::permissions::permission_mode::is_default_mode(
            context.tool_permission_context.mode
        ));
        assert!(context.tools.iter().any(|tool| tool.name == "Bash"));
    }

    #[test]
    fn tool_use_context_carries_active_tools_like_official_options() {
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_tools(vec![crate::tools::file_read_tool::file_read_tool_schema()])
            .with_non_interactive_session(true);

        assert_eq!(context.tools.len(), 1);
        assert_eq!(context.tools[0].name, "Read");
        assert!(context.is_non_interactive_session);
    }

    #[test]
    fn tool_use_context_carries_readonly_resume_payloads() {
        let read_file_state = vec![ReadFileStateEntry {
            path: "/tmp/project/src/lib.rs".to_string(),
            content: Some("fn main() {}".to_string()),
            timestamp_ms: Some(1),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        }];
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_resume_payloads(read_file_state.clone(), vec!["git".to_string()]);

        assert_eq!(context.read_file_state.snapshot(), read_file_state);
        assert_eq!(context.bash_tools, vec!["git".to_string()]);
        assert_eq!(context.resume_restore_stores.read_file_state.len(), 1);
        assert_eq!(
            context.resume_restore_stores.bash_tools,
            vec!["git".to_string()]
        );
        assert!(crate::utils::permissions::permission_mode::is_default_mode(
            context.tool_permission_context.mode
        ));
        assert!(context.terminal_notification_requests.is_empty());
        assert!(context.terminal_notification_service_plans.is_empty());
    }

    #[test]
    fn tool_use_context_carries_full_resume_restore_stores_without_side_effects() {
        let read_file_state = vec![ReadFileStateEntry {
            path: "/tmp/project/src/lib.rs".to_string(),
            content: Some("fn main() {}".to_string()),
            timestamp_ms: Some(1),
            offset: None,
            limit: None,
            is_partial_view: false,
            source: ReadFileStateSource::Read,
        }];
        let stores = ResumeRestoreStores {
            session_id: Some("session-1".to_string()),
            project_path: Some("/tmp/project".to_string()),
            file_history: Some(RestoredFileHistoryState {
                snapshots: vec![serde_json::json!({"messageId": "message-1"})],
                tracked_files: vec!["src/lib.rs".to_string()],
                snapshot_sequence: 1,
            }),
            context_collapse: RestoredContextCollapseState {
                commits: vec![serde_json::json!({"sessionId": "session-1"})],
                snapshot: Some(serde_json::json!({"sessionId": "session-1"})),
            },
            read_file_state: read_file_state.clone(),
            bash_tools: vec!["git".to_string()],
            ..ResumeRestoreStores::default()
        };

        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_resume_restore_stores(stores.clone());

        assert_eq!(context.read_file_state.snapshot(), read_file_state);
        assert_eq!(context.bash_tools, vec!["git".to_string()]);
        assert_eq!(context.resume_restore_stores, stores);
        assert!(context.terminal_notification_requests.is_empty());
        assert!(context.terminal_notification_service_plans.is_empty());
    }

    #[test]
    fn file_read_context_clone_shares_each_identity_independently() {
        let context = ToolUseContext::default();
        let cloned = context.clone();
        assert!(
            context
                .read_file_state
                .same_identity(&cloned.read_file_state)
        );
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .same_identity(cloned.nested_memory_attachment_triggers.as_ref().unwrap())
        );
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .same_identity(cloned.dynamic_skill_dir_triggers.as_ref().unwrap())
        );
    }

    #[test]
    fn file_read_handles_support_source_constructor_identity_matrix() {
        let context = ToolUseContext::default();

        // QueryEngine/REPL shape: same cache with fresh trigger Sets.
        let mut same_cache_fresh_sets = context.clone();
        same_cache_fresh_sets.nested_memory_attachment_triggers = context
            .nested_memory_attachment_triggers
            .as_ref()
            .map(|_| SharedOrderedTriggerSet::fresh());
        same_cache_fresh_sets.dynamic_skill_dir_triggers = context
            .dynamic_skill_dir_triggers
            .as_ref()
            .map(|_| SharedOrderedTriggerSet::fresh());
        assert!(
            context
                .read_file_state
                .same_identity(&same_cache_fresh_sets.read_file_state)
        );
        assert!(
            !context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .same_identity(
                    same_cache_fresh_sets
                        .nested_memory_attachment_triggers
                        .as_ref()
                        .unwrap()
                )
        );
        assert!(
            !context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .same_identity(
                    same_cache_fresh_sets
                        .dynamic_skill_dir_triggers
                        .as_ref()
                        .unwrap()
                )
        );

        // MagicDocs shape: cloned cache contents with inherited Set identities.
        let mut cloned_cache_inherited_sets = context.clone();
        cloned_cache_inherited_sets.read_file_state = context.read_file_state.cloned_contents();
        assert!(
            !context
                .read_file_state
                .same_identity(&cloned_cache_inherited_sets.read_file_state)
        );
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .same_identity(
                    cloned_cache_inherited_sets
                        .nested_memory_attachment_triggers
                        .as_ref()
                        .unwrap()
                )
        );
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .same_identity(
                    cloned_cache_inherited_sets
                        .dynamic_skill_dir_triggers
                        .as_ref()
                        .unwrap()
                )
        );
    }

    #[test]
    fn file_read_source_turn_serializes_cross_field_commits() {
        let context = std::sync::Arc::new(ToolUseContext::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = std::sync::Arc::clone(&context);
        let first_thread = std::thread::spawn(move || {
            FileReadSourceTurn::run(|| {
                first.read_file_state.with_cache_in_source_turn(|cache| {
                    cache.set_entry(ReadFileStateEntry {
                        path: "/tmp/first".to_string(),
                        content: Some("first".to_string()),
                        timestamp_ms: Some(1),
                        offset: Some(serde_json::json!(1)),
                        limit: None,
                        is_partial_view: false,
                        source: ReadFileStateSource::Read,
                    });
                });
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                first
                    .nested_memory_attachment_triggers
                    .as_ref()
                    .unwrap()
                    .with_set_in_source_turn(|set| {
                        set.insert("/tmp/first".to_string());
                    });
            });
        });
        entered_rx.recv().unwrap();

        let second = std::sync::Arc::clone(&context);
        let second_thread = std::thread::spawn(move || {
            FileReadSourceTurn::run(|| {
                second.read_file_state.with_cache_in_source_turn(|cache| {
                    cache.set_entry(ReadFileStateEntry {
                        path: "/tmp/second".to_string(),
                        content: Some("second".to_string()),
                        timestamp_ms: Some(2),
                        offset: Some(serde_json::json!(1)),
                        limit: None,
                        is_partial_view: false,
                        source: ReadFileStateSource::Read,
                    });
                });
                second
                    .nested_memory_attachment_triggers
                    .as_ref()
                    .unwrap()
                    .with_set_in_source_turn(|set| {
                        set.insert("/tmp/second".to_string());
                    });
            });
        });
        release_tx.send(()).unwrap();
        first_thread.join().unwrap();
        second_thread.join().unwrap();

        assert_eq!(
            context
                .read_file_state
                .snapshot()
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>(),
            vec!["/tmp/first", "/tmp/second"]
        );
        assert_eq!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .snapshot(),
            vec!["/tmp/first", "/tmp/second"]
        );
    }

    #[test]
    fn file_read_source_turn_recovers_after_panic_without_poisoning_later_operations() {
        let panic = std::panic::catch_unwind(|| {
            FileReadSourceTurn::run(|| panic!("source-turn sentinel"));
        });
        assert!(panic.is_err());

        let context = ToolUseContext::default();
        context
            .nested_memory_attachment_triggers
            .as_ref()
            .unwrap()
            .add("after-panic".to_string());
        assert!(
            context
                .nested_memory_attachment_triggers
                .as_ref()
                .unwrap()
                .contains("after-panic")
        );
    }

    #[test]
    fn file_read_source_turn_reentry_panics_without_poisoning_later_operations() {
        let reentry =
            std::panic::catch_unwind(|| FileReadSourceTurn::run(|| FileReadSourceTurn::run(|| ())));
        assert!(reentry.is_err());

        let context = ToolUseContext::default();
        context
            .dynamic_skill_dir_triggers
            .as_ref()
            .unwrap()
            .add("after-reentry".to_string());
        assert!(
            context
                .dynamic_skill_dir_triggers
                .as_ref()
                .unwrap()
                .contains("after-reentry")
        );
    }

    #[test]
    fn trigger_iteration_and_snapshot_clear_match_official_race_boundaries() {
        let nested = SharedOrderedTriggerSet::fresh();
        nested.add("first".to_string());
        assert_eq!(nested.next_or_clear(0).as_deref(), Some("first"));
        nested.add("joined-before-terminal".to_string());
        assert_eq!(
            nested.next_or_clear(1).as_deref(),
            Some("joined-before-terminal")
        );
        assert_eq!(nested.next_or_clear(2), None);
        assert!(nested.is_empty());
        nested.add("after-terminal".to_string());
        assert_eq!(nested.next_or_clear(0).as_deref(), Some("after-terminal"));

        let dynamic = SharedOrderedTriggerSet::fresh();
        dynamic.add("snapshotted".to_string());
        let snapshot = dynamic.snapshot();
        dynamic.add("added-during-io".to_string());
        assert_eq!(snapshot, vec!["snapshotted"]);
        dynamic.clear();
        assert!(dynamic.is_empty());
    }

    #[test]
    fn file_read_optional_trigger_absence_survives_clone_and_detach() {
        let mut context = ToolUseContext::default();
        context.nested_memory_attachment_triggers = None;
        context.dynamic_skill_dir_triggers = None;
        assert!(context.clone().nested_memory_attachment_triggers.is_none());
        assert!(context.clone().dynamic_skill_dir_triggers.is_none());
    }

    #[test]
    fn tool_use_context_records_terminal_notification_requests_without_side_effects() {
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let request = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Mock task complete"),
            TerminalNotificationChannel::Kitty,
            Some("kitty"),
        );

        context.record_terminal_notification_request(request);

        assert_eq!(context.terminal_notification_requests.len(), 1);
        assert_eq!(
            context.terminal_notification_requests[0].method,
            TerminalNotificationMethod::Kitty
        );
        assert_eq!(
            context.terminal_notification_requests[0].options.message,
            "Mock task complete"
        );
        assert!(context.terminal_notification_service_plans.is_empty());
    }

    #[test]
    fn tool_use_context_records_terminal_notification_service_plan_without_side_effects() {
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default());
        let mut config = GlobalConfig::default();
        config.preferred_notif_channel = Some("terminal_bell".to_string());
        let env = TerminalNotificationEnvironment {
            terminal: Some("kitty"),
            apple_terminal_bell_disabled: None,
        };

        context.record_terminal_notification_from_config(
            TerminalNotificationOptions::new("idle_prompt", "Nyan is waiting"),
            &config,
            &env,
        );

        assert_eq!(context.terminal_notification_requests.len(), 1);
        assert_eq!(context.terminal_notification_service_plans.len(), 1);
        assert_eq!(
            context.terminal_notification_requests[0].method,
            TerminalNotificationMethod::TerminalBell
        );

        let plan = &context.terminal_notification_service_plans[0];
        assert_eq!(plan.configured_channel, "terminal_bell");
        assert_eq!(plan.method_used, "terminal_bell");
        assert_eq!(plan.hook_input.hook_event_name, "Notification");
        assert_eq!(plan.hook_input.notification_type, "idle_prompt");
        assert_eq!(plan.hook_input.message, "Nyan is waiting");
    }

    #[test]
    fn tool_use_context_get_set_app_state_reaches_live_store() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = false;
        let store = crate::state::store::AppStore::new(initial, None);
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_app_store(store.clone());

        assert_eq!(context.get_app_state().map(|s| s.verbose), Some(false));
        context.set_app_state(|state| {
            state.verbose = true;
        });
        assert!(store.get().verbose);
        assert_eq!(context.get_app_state().map(|s| s.verbose), Some(true));

        context.set_app_state_for_tasks(|state| {
            state.foregrounded_task_id = Some("task-1".into());
        });
        assert_eq!(store.get().foregrounded_task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn tool_use_context_set_app_state_respects_writable_flag() {
        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = false;
        let store = crate::state::store::AppStore::new(initial, None);
        let mut context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_app_store(store.clone());
        context.app_store.writable = false;
        context.set_app_state(|state| {
            state.verbose = true;
        });
        assert!(!store.get().verbose);
        context.set_app_state_for_tasks(|state| {
            state.verbose = true;
        });
        assert!(store.get().verbose);
    }

    #[test]
    fn interruptible_tool_state_matches_all_executing_tools_rule() {
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = observed.clone();
        let mut context = ToolUseContext::default();
        context.set_has_interruptible_tool_in_progress =
            SetHasInterruptibleToolInProgress(Some(std::sync::Arc::new(move |value| {
                sink.lock().unwrap().push(value);
            })));

        context.mark_in_progress_with_behavior("cancel", InterruptBehavior::Cancel);
        assert_eq!(observed.lock().unwrap().last().copied(), Some(true));
        context.mark_in_progress_with_behavior("block", InterruptBehavior::Block);
        assert_eq!(observed.lock().unwrap().last().copied(), Some(false));
        context.mark_complete("block");
        assert_eq!(observed.lock().unwrap().last().copied(), Some(true));
        context.mark_complete("cancel");
        assert_eq!(observed.lock().unwrap().last().copied(), Some(false));
    }

    fn two_profile_settings() -> crate::utils::settings::SettingsJson {
        serde_json::from_value(serde_json::json!({
            "nyan": {
                "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                "models": {
                    "main": {"provider": "p", "model": "claude-sonnet-4-6",
                        "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}},
                    "deep": {"provider": "p", "model": "claude-opus-4-6",
                        "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000}}
                },
                "selection": {"main": "main"}
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_context_hands_its_model_effort_and_profile_to_what_inherits_it() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::settings::settings_cache::install_test_settings(two_profile_settings());

        let mut context = ToolUseContext::default()
            .with_main_loop_model("claude-opus-4-6")
            .with_effort_value(Some(crate::utils::effort::Effort::Max));
        context.main_loop_profile = Some("deep".to_string());
        let parent = context.parent_selection();
        assert_eq!(parent.model, "claude-opus-4-6");
        assert_eq!(parent.effort, Some(crate::utils::effort::Effort::Max));
        assert_eq!(parent.profile.as_deref(), Some("deep"));
        assert_eq!(context.effective_profile_name().as_deref(), Some("deep"));

        // A session that never switched runs on `selection.main`.
        let plain = ToolUseContext::default().with_main_loop_model("claude-sonnet-4-6");
        assert_eq!(plain.parent_selection().profile, None);
        assert_eq!(plain.effective_profile_name().as_deref(), Some("main"));

        crate::utils::settings::settings_cache::reset_settings_cache();
    }

    #[test]
    fn the_selected_profile_survives_the_options_round_trip_a_subagent_context_makes() {
        let mut context = ToolUseContext::default().with_main_loop_model("m");
        context.main_loop_profile = Some("deep".to_string());
        let options = context.options();
        assert_eq!(options.main_loop_profile.as_deref(), Some("deep"));
        let mut other = ToolUseContext::default();
        other.apply_options(options);
        assert_eq!(other.main_loop_profile.as_deref(), Some("deep"));
    }
}
