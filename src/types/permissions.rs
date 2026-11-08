//! Permission type definitions.
//! Maps to: CC `types/permissions.ts`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Maps to CC `types/permissions.ts` `EXTERNAL_PERMISSION_MODES`.
pub const EXTERNAL_PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "bypassPermissions",
    "default",
    "dontAsk",
    "plan",
];

/// Maps to CC `types/permissions.ts:33-38` `INTERNAL_PERMISSION_MODES`.
/// Includes internal `auto` (TRANSCRIPT_CLASSIFIER). `bubble` is part of the
/// `PermissionMode` union but deliberately absent from the user-addressable
/// runtime set, so settings/CLI/recovery can never select it.
pub const PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "bypassPermissions",
    "default",
    "dontAsk",
    "plan",
    "auto",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
    /// Deny prompts instead of asking.
    DontAsk,
    /// Bypass all permissions
    BypassPermissions,
    /// Auto mode: AI classifier replaces interactive prompts for many tools.
    /// Maps to CC internal mode `auto` (feature TRANSCRIPT_CLASSIFIER).
    Auto,
    /// Fork subagent mode: permission prompts bubble up to the parent terminal
    /// instead of being auto-denied for an async agent.
    ///
    /// Maps to CC internal mode `bubble` (`types/permissions.ts:28`). It is not
    /// in `PERMISSION_MODES`, has no `PERMISSION_MODE_CONFIG` entry, and no
    /// branch of the permission decision chain tests for it — CC only reads it
    /// in `tools/AgentTool/runAgent.ts:440-445` to keep
    /// `shouldAvoidPermissionPrompts` false for an async fork child. Everywhere
    /// else it falls back to `default`.
    Bubble,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleValue {
    pub tool_name: String,
    pub rule_content: Option<String>,
}

/// Maps to: CC `types/permissions.ts:52-78#PermissionRuleSource`.
/// @nyan: no `policySettings` (no managed policy layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionRuleSource {
    #[serde(rename = "userSettings")]
    UserSettings,
    #[serde(rename = "projectSettings")]
    ProjectSettings,
    #[serde(rename = "localSettings")]
    LocalSettings,
    #[serde(rename = "flagSettings")]
    FlagSettings,
    #[serde(rename = "cliArg")]
    CliArg,
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "session")]
    Session,
}

/// Maps to CC `types/permissions.ts#PermissionRule`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub source: PermissionRuleSource,
    pub rule_behavior: PermissionBehavior,
    pub rule_value: PermissionRuleValue,
}

impl PermissionRuleValue {
    pub fn new(tool_name: impl Into<String>, rule_content: Option<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            rule_content,
        }
    }

    pub fn display(&self) -> String {
        match &self.rule_content {
            Some(content) if !content.is_empty() => format!("{}({content})", self.tool_name),
            _ => self.tool_name.clone(),
        }
    }
}

/// Maps to: CC `types/permissions.ts:80-93#PermissionUpdateDestination`.
///
/// This is update routing only. `flagSettings` and `command` are live rule
/// sources but are not editable update destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionUpdateDestination {
    #[serde(rename = "userSettings")]
    UserSettings,
    #[serde(rename = "projectSettings")]
    ProjectSettings,
    #[serde(rename = "localSettings")]
    LocalSettings,
    #[serde(rename = "session")]
    Session,
    #[serde(rename = "cliArg")]
    CliArg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionUpdate {
    SetMode {
        destination: PermissionUpdateDestination,
        mode: PermissionMode,
    },
    AddRules {
        destination: PermissionUpdateDestination,
        behavior: PermissionBehavior,
        rules: Vec<PermissionRuleValue>,
    },
    ReplaceRules {
        destination: PermissionUpdateDestination,
        behavior: PermissionBehavior,
        rules: Vec<PermissionRuleValue>,
    },
    RemoveRules {
        destination: PermissionUpdateDestination,
        behavior: PermissionBehavior,
        rules: Vec<PermissionRuleValue>,
    },
    /// Maps to CC `PermissionUpdate` `{ type: 'addDirectories' }`.
    /// Editable destinations persist directory scope; session/CLI
    /// destinations remain in-memory.
    AddDirectories {
        destination: PermissionUpdateDestination,
        directories: Vec<String>,
    },
    /// Maps to CC `PermissionUpdate` `{ type: 'removeDirectories' }`.
    RemoveDirectories {
        destination: PermissionUpdateDestination,
        directories: Vec<String>,
    },
}

/// Maps to CC `types/permissions.ts#AdditionalWorkingDirectory`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalWorkingDirectory {
    pub path: String,
    pub source: PermissionRuleSource,
}

/// Maps to: CC `types/permissions.ts:413-421#ToolPermissionRulesBySource`.
pub type ToolPermissionRulesBySource = HashMap<PermissionRuleSource, Vec<PermissionRuleValue>>;

/// User-visible permission request data projected from a `ToolUseConfirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// L1 carrier for CC's complete permission decision until the UI projection.
    /// The existing request fields remain the prompt/execution view; this retains
    /// fields such as pendingClassifierCheck that are not prompt properties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_result: Option<PermissionDecision>,
    pub id: String,
    pub tool_use_id: String,
    pub tool_name: String,
    /// Carries CC `Tool.mcpInfo` to `toolMatchesRule`. CC hands that function
    /// the `Tool` itself; the Rust seam only receives a `PermissionRuleValue`,
    /// so the MCP identity has to ride along on the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_info: Option<crate::types::tools::McpToolInfo>,
    /// Maps to: CC `ToolUseConfirm.description`
    /// (`components/permissions/PermissionRequest.tsx:140`), produced once by
    /// `await tool.description(input, …)` at `hooks/useCanUseTool.tsx:138-143`
    /// and rendered dim under the tool-use line
    /// (`FallbackPermissionRequest.tsx:179`). The Rust owner of that call is
    /// `hooks/tool_permission/handlers/interactive_handler.rs#fill_tool_description`.
    ///
    /// This is the TOOL describing itself. It is NOT the decision's own
    /// explanation — that one is [`Self::message`], and CC keeps the two apart.
    pub description: String,
    /// Maps to: CC `PermissionDecision.message`, a REQUIRED field on
    /// `PermissionAskDecision` (`types/permissions.ts:203`),
    /// `PermissionDenyDecision` (`:233`) and the `passthrough` variant
    /// (`:257`) — and absent from the allow variant.
    ///
    /// This is the DECISION explaining itself, and it is model-facing: CC reads
    /// it at `services/tools/toolExecution.ts:1023`
    /// (`let errorMessage = permissionDecision.message`) and puts it in the
    /// error `tool_result` the model sees. Producers include
    /// `permissions.ts:1179` (`Permission to use X has been denied.`),
    /// `:1197`/`:1300` (`createPermissionRequestMessage`), each tool's own
    /// `checkPermissions` message, `buildYoloRejectionMessage`,
    /// `buildClassifierUnavailableMessage`, and
    /// `PermissionContext.cancelAndAbort`'s `REJECT_MESSAGE` family.
    ///
    /// Until #132 the port had only `description`, so a deny wrote its
    /// explanation over what the tool had said about itself and the model
    /// received the generic REJECT_MESSAGE instead of the real reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Rule-content projection of the tool input: this is what
    /// `PermissionRuleValue::rule_content` is built from, and what
    /// `always_allow_rules` are matched against. It is NOT a display string —
    /// the dialogs render each tool's own `renderToolUseMessage` instead
    /// (`FallbackPermissionRequest.tsx:166-178`).
    pub input_summary: String,
    /// Maps to official `ToolUseConfirm.input`: the structured model tool
    /// input used by permission handlers and actual tool execution. UI can
    /// still render `input_summary`, but executors should prefer this field.
    pub input: serde_json::Value,
    /// Internal L1 transport for CC `toolExecution.ts`'s `callInput`: the
    /// un-backfilled model input used by `tool.call(...)` unless a hook or user
    /// explicitly changes the observable `file_path`.
    #[serde(skip)]
    pub call_input: Option<serde_json::Value>,
    /// The rule value this tool use *requests*, derived from the input (or
    /// lifted from a `checkPermissions` suggestion). Two owners read it:
    /// `getDenyRuleForTool`/`getAskRuleForTool`/`toolAlwaysAllowedRule` match
    /// against it, and "always allow" turns it into an `addRules` update.
    ///
    /// It is deliberately NOT the rule a permission explanation may quote —
    /// that one is the *matched* rule inside [`Self::decision_reason`]. The two
    /// were the same field until the #124 fix, which made every dialog claim
    /// "Permission rule <current input> requires confirmation".
    pub rule: PermissionRuleValue,
    /// Maps to: CC `ToolUseConfirm.permissionResult.decisionReason`
    /// (`PermissionRequest.tsx:139`), the sole input of
    /// `PermissionRuleExplanation`
    /// (`PermissionRuleExplanation.tsx:91-97` — `null` strings render nothing).
    ///
    /// `None` is CC's "no decisionReason", which is exactly what the step-3
    /// `passthrough → ask` conversion produces for a tool that returned the
    /// `TOOL_DEFAULTS` passthrough seed (`permissions.ts:1299-1310`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<PermissionDecisionReason>,
    /// Projection of CC `ToolUseConfirm.permissionResult.suggestions`; Bash
    /// permission UI must retain all per-subcommand rules, not only `rule`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<PermissionUpdate>,
    /// Maps to: CC `ToolUseConfirm.permissionResult.blockedPath`
    /// (`PermissionAskDecision.blockedPath`, `types/permissions.ts:207`).
    /// Produced by the Bash/PowerShell path validators
    /// (`BashTool/pathValidation.ts:690`, `:986`) and read by two consumers:
    /// `interactiveHandler.ts:252` forwards it to the CCR bridge, and
    /// `cli/structuredIO.ts:596` sends it as the `blocked_path` key of the
    /// `can_use_tool` control request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_path: Option<String>,
    /// Maps to: CC `ToolUseConfirm.permissionResult.metadata`
    /// (`PermissionAskDecision.metadata`, `types/permissions.ts:208`).
    ///
    /// One producer — `SkillTool.ts:576` `metadata: commandObj ? { command:
    /// commandObj } : undefined` — and one consumer:
    /// `SkillPermissionRequest.tsx:47-52` narrows it with `'command' in …` and
    /// `:236` renders `commandObj?.description`, the SKILL's own description.
    /// That is neither [`Self::description`] (the tool describing itself,
    /// `Execute skill: ${skill}`) nor [`Self::message`] (the decision's
    /// explanation), which is why neither could stand in for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<crate::utils::permissions::permission_result::PermissionMetadata>,
    /// Projection of `decisionReason.type === 'subcommandResults'` used by the
    /// Bash dialog's compound-command prefix behavior.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_compound_command: bool,
    pub mode: PermissionMode,
}

/// Maps to CC `components/permissions/WorkerBadge.tsx#WorkerBadgeProps` as
/// carried by `ToolUseConfirm.workerBadge`.  The UI component resolves this
/// string color into a terminal color when rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionWorkerBadge {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Mailbox response target for pane/out-of-process worker permission prompts.
/// Maps to CC `useInboxPoller.ts` ToolUseConfirm callbacks that call
/// `sendPermissionResponseViaMailbox(parsed.agent_id, ..., parsed.request_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxPermissionResponseTarget {
    pub worker_name: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

/// How a settled row hands its decision back to the pending `canUseTool`
/// promise. CC has one form — the `resolve` of the promise the asking
/// `canUseTool` returned, closed over by the queued entry
/// (`interactiveHandler.ts:57-60`, `inProcessRunner.ts:199`) — because CC's
/// asker and the queue live in one process on one event loop. This port
/// evaluates permissions on query-actor threads, so the same `resolve` takes
/// two transports depending on which actor is parked on the promise.
#[derive(Clone)]
enum PermissionResponseDelivery {
    /// The asking query is NOT the one the REPL drives (a subagent's or
    /// teammate's nested query): the promise is the `bounded(1)` hand-off
    /// `interactive_handler.rs#create_repl_interactive_permission_sink` awaits.
    Channel(async_channel::Sender<PermissionPromptResponse>),
    /// The asking query IS the one the REPL drives: the promise lives inside
    /// that actor and is settled by `QueryCommand::PermissionResponse` on the
    /// command channel of the handle that raised THIS row — not on whichever
    /// query the REPL happens to consider active when the answer arrives.
    Command(
        #[allow(clippy::type_complexity)]
        Arc<dyn Fn(PermissionPromptResponse) -> bool + Send + Sync>,
    ),
}

impl PermissionResponseDelivery {
    fn deliver(&self, response: PermissionPromptResponse) -> bool {
        match self {
            Self::Channel(sender) => sender.try_send(response).is_ok(),
            Self::Command(send) => send(response),
        }
    }
}

/// Maps to: CC `hooks/toolPermission/PermissionContext.ts:63-94` `ResolveOnce`
/// / `createResolveOnce(resolve)` — the resolve-once guard every queued
/// dialog's callbacks share:
///
/// ```ts
/// function createResolveOnce<T>(resolve: (value: T) => void): ResolveOnce<T> {
///   let claimed = false
///   let delivered = false
///   return {
///     resolve(value) { if (delivered) return; delivered = true; claimed = true; resolve(value) },
///     isResolved() { return claimed },
///     claim() { if (claimed) return false; claimed = true; return true },
///   }
/// }
/// ```
///
/// `claim()` is the atomic check-and-mark CC runs BEFORE any side effect in
/// every racer: `onAbort` (`interactiveHandler.ts:138`), `onAllow` (`:160`,
/// commented "atomic check-and-mark before await"), `onReject` (`:184`),
/// `recheckPermission` (`:222`), the bridge response (`:259`), the channel
/// relay (`:366`), the hook (`:423`) and the classifier (`:455`). The loser
/// returns having done nothing at all — it does not persist, does not write the
/// live context, and does not withdraw the row (the winner already did).
/// `inProcessRunner.ts:200,241,256,293,306,315` is the same guard spelled as a
/// plain `decisionMade` boolean, and it too is set before
/// `persistPermissionUpdates` (`:263`).
///
/// `Arc` so a claim taken on one clone of a queue row is visible on every other
/// clone: CC's racers close over one `createResolveOnce` object, and this port's
/// racers each hold a `ToolUseConfirm` clone (the sweep clones the row, the REPL
/// clones the queue).
///
/// The `claimed`/`delivered` pair itself is
/// `hooks/tool_permission/permission_context.rs#ResolveOnce`, the port of
/// `createResolveOnce` that already lives in the file `PermissionContext.ts`
/// maps to. It had no production caller before, because CC's
/// `createResolveOnce(resolve)` takes the promise's `resolve` and the port's
/// copy takes none — the transport is what this struct adds, and the value CC
/// hands to `resolve` is parked in the guard's own slot and taken back out by
/// the winner (`guard.resolve(...)` then `guard.take()`).
struct PermissionResolveOnce {
    guard: crate::hooks::tool_permission::permission_context::ResolveOnce<PermissionPromptResponse>,
    delivery: PermissionResponseDelivery,
}

/// Maps to: CC `ToolUseConfirm.onAllow(...)` / `onReject(...)`
/// (`components/permissions/PermissionRequest.tsx:158-164`) — the per-entry
/// resolver CC's `useCanUseTool` closure installs when it queues a dialog, so
/// the answer goes back to *that* pending `canUseTool` promise rather than to
/// whichever query the REPL happens to consider active — plus the
/// `createResolveOnce` guard those callbacks share
/// ([`PermissionResolveOnce`]).
///
/// Nyan only needed the per-entry form once a subagent's query could raise a
/// dialog: `QueryCommand::PermissionDecision` addresses the REPL's own
/// `active_query` actor, which is not the actor that asked when the request came
/// from a subagent (`tools/AgentTool/runAgent.ts:753` hands the parent's
/// `canUseTool` to the subagent's `query(...)`). The entry itself is unchanged —
/// CC has one `ToolUseConfirm` type and one queue for every asking caller.
#[derive(Clone, Default)]
pub struct PermissionPromptResponder(Option<Arc<PermissionResolveOnce>>);

impl PermissionPromptResponder {
    /// The nested-query transport: CC's `resolve` for a promise another actor
    /// is parked on.
    pub fn new(sender: async_channel::Sender<PermissionPromptResponse>) -> Self {
        Self::with_delivery(PermissionResponseDelivery::Channel(sender))
    }

    /// The REPL's own query transport: CC's `resolve` for a promise the REPL's
    /// `active_query` actor is parked on, settled with
    /// `QueryCommand::PermissionResponse`. `send` returns whether the command
    /// reached the actor, which is this transport's "a waiter got it".
    pub fn from_command_sink<F>(send: F) -> Self
    where
        F: Fn(PermissionPromptResponse) -> bool + Send + Sync + 'static,
    {
        Self::with_delivery(PermissionResponseDelivery::Command(Arc::new(send)))
    }

    fn with_delivery(delivery: PermissionResponseDelivery) -> Self {
        Self(Some(Arc::new(PermissionResolveOnce {
            guard: crate::hooks::tool_permission::permission_context::create_resolve_once(),
            delivery,
        })))
    }

    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }

    /// Maps to: CC `PermissionContext.ts:88-92` `claim()` — "Atomically
    /// check-and-mark as resolved. Returns true if this caller won the race
    /// (nobody else has resolved yet), false otherwise."
    ///
    /// A row with NO resolver is the scripted `pending_responses` runtime's
    /// (`run_tools_for_message` queues it with no asking actor behind it), and
    /// it has no second racer — so there is nothing to lose to and the caller
    /// may proceed. CC has no resolver-less row.
    pub fn claim(&self) -> bool {
        match &self.0 {
            Some(inner) => inner.guard.claim(),
            None => true,
        }
    }

    /// Maps to: CC `PermissionContext.ts:85-87` `isResolved()`.
    pub fn is_resolved(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|inner| inner.guard.is_resolved())
    }

    /// Maps to: CC `PermissionContext.ts:79-84` `resolve(value)` — settle the
    /// pending `canUseTool` promise this entry stands for, at most once
    /// (`if (delivered) return`), marking the guard claimed on the way
    /// (`claimed = true`), so a racer that skipped `claim()` cannot be
    /// overtaken afterwards.
    ///
    /// Returns false when no responder is attached (the scripted runtime's
    /// rows), when a decision was already delivered, or when the waiter is
    /// gone.
    pub fn respond(&self, response: PermissionPromptResponse) -> bool {
        let Some(inner) = &self.0 else {
            return false;
        };
        // CC `if (delivered) return; delivered = true; claimed = true`.
        if !inner.guard.resolve(response) {
            return false;
        }
        // CC's `resolve(value)` hands `value` straight to the promise's own
        // `resolve`; here the winner takes it back out of the guard and hands
        // it to whichever transport this row's asker is parked on.
        let Some(response) = inner.guard.take() else {
            return false;
        };
        inner.delivery.deliver(response)
    }
}

impl std::fmt::Debug for PermissionPromptResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "PermissionPromptResponder(set)"
        } else {
            "PermissionPromptResponder(unset)"
        })
    }
}

impl PartialEq for PermissionPromptResponder {
    // Queue entries compare on responder presence; the resolver is identity-free.
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

impl Eq for PermissionPromptResponder {}

/// This currently stores the UI request projection and official queue metadata.
/// `onAllow`/`onReject` are `responder` when the asking query owns the pending
/// promise (a subagent's query), `QueryCommand::PermissionDecision` for the
/// REPL's own active query, and `mailbox_response_target` for out-of-process
/// teammate prompts.
///
/// `onAbort` / `recheckPermission` (CC `PermissionRequest.tsx:158-165`) are not
/// fields here — they are BEHAVIORS, and the carrier they were waiting on
/// (`utils/swarm/leaderPermissionBridge.ts#getLeaderToolUseConfirmQueue`, the
/// queue UPDATER that reaches a queued row from outside the REPL) is now ported
/// at `utils/swarm/leader_permission_bridge.rs`. What landed:
///
/// - **Withdrawal.** `SetToolUseConfirmQueueFn::remove_from_queue` is CC's
///   `setToolUseConfirmQueue(queue => queue.filter(item => item.toolUseID !==
///   toolUseID))` (`inProcessRunner.ts:214-216`, `:321-323`;
///   `ctx.removeFromQueue()` in `interactiveHandler.ts`). The abort half is
///   wired: `in_process_runner.rs#teammate_leader_dialog_sink` withdraws its
///   own row when the turn aborts (CC `:209-217`), so the human no longer sees
///   a prompt for a tool use nobody is waiting on.
/// - **recheckPermission.** The body is
///   `hooks/tool_permission/handlers/interactive_handler.rs#recheck_permission`
///   (CC `interactiveHandler.ts:204-231` and `inProcessRunner.ts:305-330` — one
///   function because this port has one entry type, but the two CC copies do
///   NOT agree on what they resolve with, so the difference is parameterised
///   off [`PermissionRowSource`]). Its CONSUMER is
///   `leader_permission_bridge.rs#recheck_queued_permissions`, CC's
///   `REPL.tsx:3114-3126` / `useReplBridge.tsx:546-554` sweep, fired from
///   `AppStore::set_tool_permission_context` (`state/store.rs`) — the single
///   choke point every in-session context write goes through here.
///
/// - **The resolve-once claim.** [`PermissionPromptResponder`] is CC's
///   `createResolveOnce` object (`PermissionContext.ts:63-94`), so every racer
///   for a row — the dialog answer, the sweep's `recheckPermission`, a
///   teammate's abort — takes the same atomic `claim()` before it persists,
///   writes the live context, or resolves.
///
/// The REPL's own prompts used to carry no resolver at all, which left the
/// sweep unable to settle them (`interactiveHandler.ts:204-231` had nothing to
/// resolve) and left the answer addressed at whichever query was active when
/// the key was pressed. They now carry
/// `PermissionResponseDelivery::Command`: the same per-entry resolver, whose
/// transport is `QueryCommand::PermissionResponse` on the command channel of
/// the handle that RAISED the row (`screens/repl.rs`, `QueryEvent::
/// PermissionRequest`). Rows with no resolver are now only the scripted
/// `pending_responses` runtime's, which has no second racer.
/// Maps to: CC `components/permissions/PermissionRequest.tsx` / `ToolUseConfirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUseConfirm {
    pub request: PermissionRequest,
    /// Maps to: CC `ToolUseConfirm.toolUseContext`
    /// (`components/permissions/PermissionRequest.tsx:142`) — the ASKER's
    /// `ToolUseContext`, installed by whoever pushed the row
    /// (`interactiveHandler.ts:97` `toolUseContext: ctx.toolUseContext`,
    /// `inProcessRunner.ts:230` `toolUseContext`), narrowed here to the one
    /// slice a re-evaluation reads.
    ///
    /// CC's only consumer of the field that can CHANGE a decision is
    /// `recheckPermission` (`interactiveHandler.ts:204-212`,
    /// `inProcessRunner.ts:305-313`), which hands it to
    /// `hasPermissionsToUseTool`. That function touches exactly two things on
    /// `context.getAppState()`: `toolPermissionContext` and `denialTracking`
    /// (ast-grep `appState.$FIELD` over
    /// `utils/permissions/permissions.ts` — every hit is one of those two).
    /// `denialTracking` is the leader's live store on both sides, because an
    /// agent's `agentGetAppState` returns `{...state, toolPermissionContext,
    /// effortValue}` (`runAgent.ts:493-497`) — it replaces the permission
    /// context and nothing else the permission system reads. So
    /// `ToolPermissionContext` is the complete carrier, not an approximation.
    ///
    /// `None` means "the leader's live context IS the asking context", which is
    /// the REPL's own rows: CC gives them the REPL's `toolUseContext`, whose
    /// `getAppState` is the untransformed store read.
    ///
    /// Why it must ride the row rather than be re-derived: `agentGetAppState`
    /// both NARROWS (`runAgent.ts:421-434`, a `plan` agent under a `default`
    /// leader) and WIDENS-away (`:469-478` REPLACES `alwaysAllowRules` with
    /// `{cliArg, session}`, dropping the parent's user/project/local/flag/
    /// policy/command/session allows). Re-evaluating a subagent's or teammate's
    /// row against the leader's live context could therefore resolve to an
    /// `allow` the asking agent's own context would never have produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking_tool_permission_context: Option<crate::tool::ToolPermissionContext>,
    /// Maps to CC `ToolUseConfirm.permissionPromptStartTimeMs`.
    pub permission_prompt_start_time_ms: Option<i64>,
    /// Maps to CC `ToolUseConfirm.classifierCheckInProgress`.
    pub classifier_check_in_progress: bool,
    /// Maps to CC `ToolUseConfirm.workerBadge`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_badge: Option<PermissionWorkerBadge>,
    /// Maps to CC pane-worker `onAllow`/`onReject` mailbox callbacks in
    /// `hooks/useInboxPoller.ts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_response_target: Option<MailboxPermissionResponseTarget>,
    /// Maps to CC `ToolUseConfirm.onAllow`/`onReject`
    /// (`PermissionRequest.tsx:158-164`) for prompts whose pending
    /// `canUseTool` promise is owned by a query the REPL is not driving.
    #[serde(skip)]
    pub responder: PermissionPromptResponder,
    /// Which of CC's two `ToolUseConfirm` builders produced this row.
    ///
    /// See [`PermissionRowSource`]: CC's discriminator is structural — WHICH
    /// closure built the row — and this port's rows are data, so the fact has
    /// to ride on the row.
    #[serde(default, skip_serializing_if = "PermissionRowSource::is_interactive")]
    pub source: PermissionRowSource,
}

/// Which of CC's two `ToolUseConfirm` builders produced a queued row.
///
/// CC has ONE row type and ONE queue (`REPL.tsx:1529`), but two constructors,
/// and the callbacks they install are not the same function twice. Two of the
/// differences are behavioural and both are read off this one fact, exactly as
/// CC reads them off "which closure am I in":
///
/// 1. **The permission-update write-back.** `inProcessRunner.ts:263-281`
///    persists to disk and hands the leader
///    `setToolPermissionContext(updatedContext, { preserveMode: true })`,
///    "to prevent workers' transformed 'acceptEdits' context from leaking back
///    to the coordinator". Every other producer writes back through
///    `PermissionContext.ts:139-147` `persistPermissions`, which passes no
///    options and therefore adopts the incoming mode.
/// 2. **What `recheckPermission` resolves with.** `interactiveHandler.ts:229`
///    is `ctx.buildAllow(freshResult.updatedInput ?? ctx.input)`;
///    `inProcessRunner.ts:324-328` is
///    `resolve({ ...freshResult, updatedInput: input, userModified: false })`,
///    where the explicit key OVERRIDES the spread. See
///    `interactive_handler.rs#recheck_permission`.
///
/// Stamped by `tool_use_confirm_for_request` from the in-process teammate task
/// scope, which is exactly the condition under which CC's
/// `createInProcessCanUseTool` is the builder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRowSource {
    /// CC `hooks/toolPermission/handlers/interactiveHandler.ts:92-232` — the
    /// row `useCanUseTool`'s `ask` leg pushes, for the REPL's own tools and for
    /// every subagent whose `canUseTool` is the parent's closure.
    #[default]
    Interactive,
    /// CC `utils/swarm/inProcessRunner.ts:223-332` — the row
    /// `createInProcessCanUseTool` pushes for an in-process teammate.
    InProcessTeammate,
}

impl PermissionRowSource {
    fn is_interactive(&self) -> bool {
        matches!(self, Self::Interactive)
    }
}

impl ToolUseConfirm {
    pub fn new(request: PermissionRequest) -> Self {
        Self {
            request,
            asking_tool_permission_context: None,
            permission_prompt_start_time_ms: None,
            classifier_check_in_progress: false,
            worker_badge: None,
            mailbox_response_target: None,
            responder: PermissionPromptResponder::default(),
            source: PermissionRowSource::Interactive,
        }
    }

    /// Maps to CC `useCanUseTool.tsx:70,307-324` — the queued entry closes over
    /// the `resolve` of the promise `canUseTool` returned to its caller.
    pub fn with_responder(mut self, responder: PermissionPromptResponder) -> Self {
        self.responder = responder;
        self
    }

    /// Maps to: CC `interactiveHandler.ts:97` / `inProcessRunner.ts:230`
    /// `toolUseContext: <the asker's>` on the pushed `ToolUseConfirm`.
    pub fn with_asking_tool_permission_context(
        mut self,
        context: Option<crate::tool::ToolPermissionContext>,
    ) -> Self {
        self.asking_tool_permission_context = context;
        self
    }

    pub fn with_permission_prompt_start_time_ms(mut self, timestamp_ms: i64) -> Self {
        self.permission_prompt_start_time_ms = Some(timestamp_ms);
        self
    }

    pub fn with_worker_badge(mut self, worker_badge: PermissionWorkerBadge) -> Self {
        self.worker_badge = Some(worker_badge);
        self
    }

    pub fn with_mailbox_response_target(mut self, target: MailboxPermissionResponseTarget) -> Self {
        self.mailbox_response_target = Some(target);
        self
    }

    pub fn tool_use_id(&self) -> &str {
        &self.request.tool_use_id
    }

    /// Whether this row's permission-update write-back must keep the LEADER's
    /// mode — CC `inProcessRunner.ts:277-279` `{ preserveMode: true }`, which
    /// only the in-process teammate's builder passes.
    pub fn preserves_leader_permission_mode(&self) -> bool {
        matches!(self.source, PermissionRowSource::InProcessTeammate)
    }
}

/// The dialog/host answer space. Maps to: CC's `canUseTool` resolution values —
/// allow (once / with a persisted rule) or deny. CC has NO cancel answer:
/// cancellation is the pre-`canUseTool` abort gate (`toolExecution.ts:415-453`,
/// CANCEL_MESSAGE), never a permission response. A `Cancel` variant used to
/// live here; both of its producers (the REPL dialog in #138, the SDK abort
/// path in #143) were remaps of CC deny/abort semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionPromptChoice {
    AllowOnce,
    Deny,
    AlwaysAllow,
}

/// Maps to: CC `hooks/toolPermission/PermissionContext.ts:157-172,269-316`
/// Anthropic `ContentBlockParam` callback values.
/// Permission UIs currently produce image blocks, but the text variant keeps
/// the callback transport faithful to the official content-block boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionContentBlock {
    Text { text: String },
    Image { source: PermissionMediaSource },
}

/// Maps to: CC Anthropic `Base64ImageSource` used by
/// `AskUserQuestionPermissionRequest.tsx:592-600`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionMediaSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

impl PermissionContentBlock {
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: PermissionMediaSource {
                kind: "base64".to_string(),
                media_type: media_type.into(),
                data: data.into(),
            },
        }
    }
}

/// UI response to a permission prompt. Most permission dialogs only return a
/// choice, but interactive tools such as AskUserQuestion also return the
/// official `updatedInput` and content blocks that must be passed to tool execution.
/// Maps to: CC `hooks/toolPermission/PermissionContext.ts:269-316`
/// `onAllow(updatedInput, ..., feedback, contentBlocks)` / reject payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPromptResponse {
    pub choice: PermissionPromptChoice,
    pub updated_input: Option<serde_json::Value>,
    /// Maps to CC `ToolUseConfirm.onAllow(updatedInput, permissionUpdates, ...)`.
    /// These updates come from tool-specific permission dialogs (for example
    /// Bash prefix rules, WebFetch domain rules, and file-session updates) and
    /// must be applied to the in-session `ToolPermissionContext` before the
    /// approved tool executes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_updates: Vec<PermissionUpdate>,
    /// Maps to CC `ToolUseConfirm.onAllow(..., feedback)` and
    /// `ToolUseConfirm.onReject(feedback)`. Shell permission dialogs populate
    /// this when the user amends an allow/reject decision with instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// Top-level model content appended after the tool-result block. Images
    /// must remain top-level on rejected results because Anthropic rejects
    /// non-text content nested inside an error `tool_result`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_blocks: Vec<PermissionContentBlock>,
    /// Rust-side transport marker for official calls that explicitly passed the
    /// `permissionUpdates` argument, even when it was `[]`. Without this, an
    /// empty file-session update list would be indistinguishable from legacy
    /// `AlwaysAllow` choices that should fall back to adding `request.rule`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub permission_updates_explicit: bool,
    /// Maps to: CC `PermissionDecision.message` on the decision that RESOLVED
    /// the `canUseTool` promise (`types/permissions.ts:203`/`:233`), read by
    /// `services/tools/toolExecution.ts:1023`.
    ///
    /// `None` is the dialog answer: CC replaces the decision there with
    /// `PermissionContext.cancelAndAbort`'s `REJECT_MESSAGE` family
    /// (`hooks/toolPermission/PermissionContext.ts:154-172`), which
    /// `permission_terminal_result_for_decision` produces. `Some` is a system
    /// decision (rule deny, tool `checkPermissions` deny, classifier deny)
    /// carrying [`PermissionRequest::message`] forward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_message: Option<String>,
    /// Maps to: CC `PermissionDecision.decisionReason` on the decision that
    /// resolved `canUseTool`. The SDK permission-prompt normalizer always
    /// installs `{type: 'permissionPromptTool', permissionPromptToolName,
    /// toolResult}` (`PermissionPromptToolResultSchema.ts:90-94`) and CC keeps
    /// it on the resolution object (`:112-116`, `:123-126`). #170: this
    /// response transport used to flatten it away (`cli/print.rs`'s normalize
    /// match dropped it with `..`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<PermissionDecisionReason>,
    /// Maps to: CC `toolUseID` on the SDK permission decision
    /// (`PermissionPromptToolResultSchema.ts:60`/`:70`), preserved by the
    /// normalizer's `{...result}` spread (`:112-116`, `:123-126`). No CC site
    /// reads it as a property after normalization (ast-grep `$X.toolUseID`
    /// over `../rebuild/src`: every hit is a ToolUseConfirm / progress /
    /// control-payload read), but CC keeps it on the object — so does this
    /// transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl PermissionPromptResponse {
    pub fn new(choice: PermissionPromptChoice) -> Self {
        Self {
            choice,
            updated_input: None,
            permission_updates: Vec::new(),
            feedback: None,
            content_blocks: Vec::new(),
            permission_updates_explicit: false,
            decision_message: None,
            decision_reason: None,
            tool_use_id: None,
        }
    }

    /// Maps to: CC returning the SYSTEM's `PermissionDecision` (with its
    /// required `message`) from `canUseTool` instead of a dialog answer.
    pub fn with_decision_message(mut self, message: impl Into<String>) -> Self {
        let message = message.into();
        self.decision_message = (!message.is_empty()).then_some(message);
        self
    }

    /// Maps to: CC keeping `decisionReason` on the decision that resolved
    /// `canUseTool` (`PermissionPromptToolResultSchema.ts:112-116,123-126`).
    pub fn with_decision_reason(mut self, reason: Option<PermissionDecisionReason>) -> Self {
        self.decision_reason = reason;
        self
    }

    /// Maps to: CC keeping the SDK `toolUseID` on the decision object
    /// (`PermissionPromptToolResultSchema.ts:60,70` through the `...result`
    /// spread).
    pub fn with_tool_use_id(mut self, tool_use_id: Option<String>) -> Self {
        self.tool_use_id = tool_use_id;
        self
    }

    pub fn allow_once_with_input(updated_input: serde_json::Value) -> Self {
        Self {
            choice: PermissionPromptChoice::AllowOnce,
            updated_input: Some(updated_input),
            permission_updates: Vec::new(),
            feedback: None,
            content_blocks: Vec::new(),
            permission_updates_explicit: false,
            decision_message: None,
            decision_reason: None,
            tool_use_id: None,
        }
    }

    pub fn with_permission_updates(mut self, permission_updates: Vec<PermissionUpdate>) -> Self {
        self.permission_updates = permission_updates;
        self.permission_updates_explicit = true;
        self
    }

    pub fn with_feedback(mut self, feedback: impl Into<String>) -> Self {
        let feedback = feedback.into();
        self.feedback = (!feedback.trim().is_empty()).then(|| feedback.trim().to_string());
        self
    }

    pub fn with_content_blocks(mut self, content_blocks: Vec<PermissionContentBlock>) -> Self {
        self.content_blocks = content_blocks;
        self
    }

    pub fn apply_to_request(&self, mut request: PermissionRequest) -> PermissionRequest {
        if let Some(updated_input) = self.updated_input.clone() {
            request.input = updated_input;
        }
        request
    }
}

impl From<PermissionPromptChoice> for PermissionPromptResponse {
    fn from(choice: PermissionPromptChoice) -> Self {
        Self::new(choice)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Maps to: CC `types/permissions.ts:157-162#PermissionCommandMetadata` —
/// "Minimal command shape for permission metadata. This is intentionally a
/// subset of the full Command type to avoid import cycles."
///
/// `extra` is the `[key: string]: unknown` index signature CC keeps "for forward
/// compatibility": the sole producer (`SkillTool.ts:576`) hands over the WHOLE
/// resolved `Command` object, so every other property rides along structurally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionCommandMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Maps to: CC `types/permissions.ts:164-169#PermissionMetadata` —
/// `{ command: PermissionCommandMetadata } | undefined`, a single-variant union.
/// `Option<PermissionMetadata>` carries the `| undefined` half.
///
/// `untagged` because CC's union member IS the `{ command: … }` object; serde's
/// default external tagging would invent a `{"Command": …}` wrapper that has no
/// counterpart upstream. Nothing puts this on a wire today
/// (`cli/structuredIO.ts:590-602` does not send it), but the in-memory shape is
/// what `SkillPermissionRequest.tsx:50`'s `'command' in metadata` narrows on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionMetadata {
    Command { command: PermissionCommandMetadata },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingClassifierCheck {
    pub command: String,
    pub cwd: String,
    pub descriptions: Vec<String>,
}

/// `Eq` (alongside the derived `PartialEq`) so this union can ride on
/// `PermissionRequest`, which CC calls `toolUseConfirm.permissionResult` and
/// hands to `PermissionRuleExplanation`
/// (`components/permissions/FallbackPermissionRequest.tsx:183-186`).
/// `serde_json::Value` is `Eq`, so nothing here blocks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PermissionDecisionReason {
    Rule {
        rule: PermissionRule,
    },
    Mode {
        mode: PermissionMode,
    },
    SubcommandResults {
        reasons: BTreeMap<String, Box<PermissionResult>>,
    },
    PermissionPromptTool {
        permission_prompt_tool_name: String,
        tool_result: serde_json::Value,
    },
    Hook {
        hook_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    AsyncAgent {
        reason: String,
    },
    SandboxOverride {
        reason: SandboxOverrideReason,
    },
    Classifier {
        classifier: String,
        reason: String,
    },
    WorkingDir {
        reason: String,
    },
    SafetyCheck {
        reason: String,
        classifier_approvable: bool,
    },
    Other {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxOverrideReason {
    #[serde(rename = "excludedCommand")]
    ExcludedCommand,
    #[serde(rename = "dangerouslyDisableSandbox")]
    DangerouslyDisableSandbox,
}

/// Maps to: CC `types/permissions.ts:241-246#PermissionDecision` —
/// `PermissionAllowDecision | PermissionAskDecision | PermissionDenyDecision`.
/// Moved here from `utils/permissions/permission_result.rs` (#142): CC defines
/// this union HERE ("Types extracted to src/types/permissions.ts to break
/// import cycles") and `utils/permissions/PermissionResult.ts` only re-exports
/// it — which is now exactly the Rust layout, the shim included. The name was
/// previously taken by the UI-only [`PromptDecision`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum PermissionDecision {
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_modified: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_reason: Option<PermissionDecisionReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accept_feedback: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content_blocks: Vec<serde_json::Value>,
    },
    Ask {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_reason: Option<PermissionDecisionReason>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<PermissionUpdate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<PermissionMetadata>,
        #[serde(default)]
        is_bash_security_check_for_misparsing: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_classifier_check: Option<PendingClassifierCheck>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content_blocks: Vec<serde_json::Value>,
    },
    Deny {
        message: String,
        decision_reason: PermissionDecisionReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
}

/// Maps to: CC `types/permissions.ts:251-266#PermissionResult` —
/// `PermissionDecision | { behavior: 'passthrough', … }`.
///
/// The TS union is FLAT: `PermissionResult`'s allow/ask/deny values ARE
/// `PermissionAllowDecision`/`PermissionAskDecision`/`PermissionDenyDecision`
/// values, with no wrapper layer. Rust cannot share variants between two
/// enums, so the composition is expressed as field-identical arms plus the
/// lossless converters below: `From<PermissionDecision>` (a decision is
/// always a result, CC's `PermissionDecision ⊂ PermissionResult`) and
/// `TryFrom<PermissionResult>` (everything but passthrough). #142 step 3
/// closed the field divergences the duplication had accumulated — Allow
/// lacked toolUseID/acceptFeedback/contentBlocks, Ask lacked
/// updatedInput/isBashSecurityCheckForMisparsing/contentBlocks, Deny lacked
/// toolUseID.
///
/// `Eq` is required transitively: `PermissionDecisionReason::SubcommandResults`
/// nests `PermissionResult`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum PermissionResult {
    /// CC `types/permissions.ts:174-184#PermissionAllowDecision`.
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_modified: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_reason: Option<PermissionDecisionReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accept_feedback: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content_blocks: Vec<serde_json::Value>,
    },
    /// CC `types/permissions.ts:199-226#PermissionAskDecision`.
    Ask {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_reason: Option<PermissionDecisionReason>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<PermissionUpdate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_path: Option<String>,
        /// Maps to: CC `types/permissions.ts:208` `metadata?: PermissionMetadata`
        /// on `PermissionAskDecision`, which `PermissionResult` includes through
        /// `PermissionDecision` (`:241-246`, `:251-254`).
        ///
        /// The sole producer is `SkillTool.ts:576`
        /// (`metadata: commandObj ? { command: commandObj } : undefined`) and the
        /// sole consumer is `SkillPermissionRequest.tsx:47-52,236`, which renders
        /// `commandObj?.description`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<PermissionMetadata>,
        #[serde(default)]
        is_bash_security_check_for_misparsing: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_classifier_check: Option<PendingClassifierCheck>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content_blocks: Vec<serde_json::Value>,
    },
    /// CC `types/permissions.ts:231-236#PermissionDenyDecision`.
    Deny {
        message: String,
        decision_reason: PermissionDecisionReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    /// CC `types/permissions.ts:255-266` — the arm `PermissionResult` adds
    /// over the decision union.
    Passthrough {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_reason: Option<PermissionDecisionReason>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<PermissionUpdate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_classifier_check: Option<PendingClassifierCheck>,
    },
}

impl PermissionResult {
    pub fn behavior(&self) -> &'static str {
        match self {
            PermissionResult::Allow { .. } => "allow",
            PermissionResult::Ask { .. } => "ask",
            PermissionResult::Deny { .. } => "deny",
            PermissionResult::Passthrough { .. } => "passthrough",
        }
    }
}

/// CC `PermissionDecision ⊂ PermissionResult` (`types/permissions.ts:251-254`):
/// every decision IS a result, verbatim — the union is flat, so this is the
/// identity injection, field for field.
impl From<PermissionDecision> for PermissionResult {
    fn from(decision: PermissionDecision) -> Self {
        match decision {
            PermissionDecision::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            } => PermissionResult::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            },
            PermissionDecision::Ask {
                message,
                updated_input,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                is_bash_security_check_for_misparsing,
                pending_classifier_check,
                content_blocks,
            } => PermissionResult::Ask {
                message,
                updated_input,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                is_bash_security_check_for_misparsing,
                pending_classifier_check,
                content_blocks,
            },
            PermissionDecision::Deny {
                message,
                decision_reason,
                tool_use_id,
            } => PermissionResult::Deny {
                message,
                decision_reason,
                tool_use_id,
            },
        }
    }
}

/// The projection back: everything but the passthrough arm, which is the one
/// value `PermissionResult` adds over the union (`:255-266`).
impl TryFrom<PermissionResult> for PermissionDecision {
    type Error = PermissionResult;

    fn try_from(result: PermissionResult) -> Result<Self, Self::Error> {
        match result {
            PermissionResult::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            } => Ok(PermissionDecision::Allow {
                updated_input,
                user_modified,
                decision_reason,
                tool_use_id,
                accept_feedback,
                content_blocks,
            }),
            PermissionResult::Ask {
                message,
                updated_input,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                is_bash_security_check_for_misparsing,
                pending_classifier_check,
                content_blocks,
            } => Ok(PermissionDecision::Ask {
                message,
                updated_input,
                decision_reason,
                suggestions,
                blocked_path,
                metadata,
                is_bash_security_check_for_misparsing,
                pending_classifier_check,
                content_blocks,
            }),
            PermissionResult::Deny {
                message,
                decision_reason,
                tool_use_id,
            } => Ok(PermissionDecision::Deny {
                message,
                decision_reason,
                tool_use_id,
            }),
            passthrough @ PermissionResult::Passthrough { .. } => Err(passthrough),
        }
    }
}

/// Decision after applying a prompt choice. This is intentionally UI-only and
/// contains transcript text instead of executable tool continuations.
///
/// A Rust-only transport, not a CC type: CC's dialog resolves through the
/// entry's `onAllow(updatedInput, permissionUpdates, …)` / `onReject(feedback)`
/// callbacks (`PermissionRequest.tsx:158-164`) and never builds an intermediate
/// object for the applied choice. It was named `PermissionDecision` until #142,
/// which squatted on the name CC gives the REAL decision union above.
///
/// # Why this survives #156
///
/// #156 removed this type from the `CanUseToolFn`/`CanUseToolCallback` pipe
/// (which now carries the canonical [`PermissionDecision`], as CC's
/// `useCanUseTool.tsx:44-53` does). What legitimately remains is the
/// applied-choice role the doc above describes: `decision_for_choice` /
/// `apply_prompt_response` (`utils/permissions/permissions.rs`) model the
/// dialog's `onAllow`/`onReject` outcome — a `choice` plus the
/// `permissionUpdates` to apply — and the swarm relay handlers
/// (`hooks/tool_permission/handlers/{coordinator,swarm_worker}_handler.rs`)
/// model the leader's mailbox answer the same way. `choice`/`updates` have no
/// home on the canonical union (CC applies updates inside the dialog handlers
/// instead of returning them), which is exactly why this transport exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptDecision {
    pub behavior: PermissionBehavior,
    pub choice: PermissionPromptChoice,
    pub updates: Vec<PermissionUpdate>,
    pub transcript: String,
    /// Maps to CC `ToolUseConfirm.onAllow(updatedInput, ...)` — the dialog's
    /// approved input that must execute (`getUpdatedInputOrFallback`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<serde_json::Value>,
}

/// Maps to: CC `types/permissions.ts:339-344` `ClassifierUsage`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Maps to: CC `types/permissions.ts:364-368` inline `promptLengths` object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierPromptLengths {
    pub system_prompt: u64,
    pub tool_calls: u64,
    pub user_prompts: u64,
}

/// Maps to: CC `types/permissions.ts:346-400` `YoloClassifierResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoloClassifierResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    pub should_block: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transcript_too_long: bool,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ClassifierUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_lengths: Option<ClassifierPromptLengths>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_dump_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage1_usage: Option<ClassifierUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage1_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage1_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage1_msg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage2_usage: Option<ClassifierUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage2_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage2_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage2_msg_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_call_input_is_private_transport_only() {
        let request = PermissionRequest {
            permission_result: None,
            id: "permission".to_string(),
            tool_use_id: "toolu_write".to_string(),
            tool_name: "Write".to_string(),
            mcp_info: None,
            description: String::new(),
            message: String::new(),
            input_summary: "/absolute/file.txt".to_string(),
            input: serde_json::json!({
                "file_path": "/absolute/file.txt",
                "content": "new"
            }),
            call_input: Some(serde_json::json!({
                "file_path": "relative/file.txt",
                "content": "new"
            })),
            rule: PermissionRuleValue::new("Write", Some("/absolute/file.txt".to_string())),
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        };
        let serialized = serde_json::to_value(&request).unwrap();
        assert!(serialized.get("callInput").is_none());
        assert!(!serialized.to_string().contains("relative/file.txt"));
        let restored: PermissionRequest = serde_json::from_value(serialized).unwrap();
        assert!(restored.call_input.is_none());
    }

    #[test]
    fn permission_update_destination_serde_has_only_five_upstream_values() {
        for (destination, serialized) in [
            (
                PermissionUpdateDestination::UserSettings,
                "\"userSettings\"",
            ),
            (
                PermissionUpdateDestination::ProjectSettings,
                "\"projectSettings\"",
            ),
            (
                PermissionUpdateDestination::LocalSettings,
                "\"localSettings\"",
            ),
            (PermissionUpdateDestination::Session, "\"session\""),
            (PermissionUpdateDestination::CliArg, "\"cliArg\""),
        ] {
            assert_eq!(serde_json::to_string(&destination).unwrap(), serialized);
            assert_eq!(
                serde_json::from_str::<PermissionUpdateDestination>(serialized).unwrap(),
                destination
            );
        }
        assert!(serde_json::from_str::<PermissionUpdateDestination>("\"command\"").is_err());
        assert!(serde_json::from_str::<PermissionUpdateDestination>("\"flagSettings\"").is_err());
    }

    #[test]
    fn permission_mode_serializes_to_official_external_strings() {
        assert_eq!(
            serde_json::to_string(&PermissionMode::Default).unwrap(),
            "\"default\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionMode::AcceptEdits).unwrap(),
            "\"acceptEdits\""
        );
        assert_eq!(
            serde_json::from_str::<PermissionMode>("\"dontAsk\"").unwrap(),
            PermissionMode::DontAsk
        );
        assert_eq!(
            serde_json::from_str::<PermissionMode>("\"bypassPermissions\"").unwrap(),
            PermissionMode::BypassPermissions
        );
    }
}
