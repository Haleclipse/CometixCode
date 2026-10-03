//! The SDK control protocol over stdio — Maps to: CC `cli/structuredIO.ts`.
//!
//! CC's owner is the `StructuredIO` class (`structuredIO.ts:135-774`) plus the
//! free functions declared beside it: `serializeDecisionReason` (`:64-91`),
//! `buildRequiresActionDetails` (`:93-117`), `exitWithMessage` (`:776-781`) and
//! `executePermissionRequestHooksForSDK` (`:787-859`). `cli/print.ts`
//! constructs one instance and calls into it; here `crate::cli::print` does the
//! same through [`SdkControlBridge`], which stands in for the class's
//! `pendingRequests` map (`:137`) and — for the control legs only — the single
//! sink its `outbound` queue (`:162`) fronts. The queue itself is not
//! reproduced; [`SdkControlBridge::emit`] states what that does and does not
//! carry over.
//!
//! Ported: `createCanUseTool`'s permission race
//! ([`request_sdk_tool_permission`], CC `:533-659`) with its hook leg
//! ([`execute_permission_request_hooks_for_sdk`], CC `:787-859`), its
//! `can_use_tool` payload ([`can_use_tool_control_request`], CC `:590-602`) and
//! its response handling ([`permission_prompt_response_from_control`], CC
//! `:611-649`); `createHookCallback` ([`create_hook_callback`], CC `:661-689`);
//! `handleElicitation` ([`request_sdk_elicitation`], CC `:694-721`); and the
//! `inputClosed` pair that belongs to the class rather than to the loop —
//! `read()`'s close tail ([`SdkControlBridge::close_input`], CC `:254-260`)
//! and its only reader, `sendRequest`'s guard
//! ([`SdkControlBridge::register_pending`], CC `:480-482`).
//!
//! The stdin loop is split: `crate::cli::print` owns it, because `processLine`
//! blends with `print.ts`-owned message→history conversion (MODULE_MAP row 64).
//! Ported there: `processLine` itself
//! ([`crate::cli::print`]`#parse_stream_json_line`, CC `:333-463`) and the
//! SPLITTING half of `read()` ([`crate::cli::print`]`#next_stream_json_line`,
//! CC `:228-231` + `:248-253` — cut at `indexOf('\n')`, keep everything else
//! including a CRLF's `\r`, process an unterminated tail).
//!
//! What remains unported of `read()` is part of its BUFFERING half, and each
//! piece is now a stated reason rather than a hole:
//! - The `content` accumulator (`:216`/`:245`) needs no port. `read_until(b'\n',
//!   …)` already reassembles a line across arrival boundaries, so no
//!   chunk-boundary split can cut a message in two. What CC's version adds is a
//!   LOSSY per-chunk `Buffer`→string decode, which splices U+FFFD into
//!   undecodable input and lets `jsonParse` decide; this port reports one bad
//!   line and keeps reading. That single divergence is stated where it lives,
//!   at `crate::cli::print`'s `next_stream_json_line`.
//! - The `prependedLines` re-check inside the split loop (`:224-227`) has no
//!   producer here: `prependUserMessage` (`:204-213`) is not ported.
//! - The `cli_stdin_message_parsed` diagnostics emission (`:234-236`) has no
//!   `logForDiagnosticsNoPII` counterpart in this port.
//!
//! The close tail (`:254-260`) IS ported, as [`SdkControlBridge::close_input`]
//! — the reader thread calls it when stdin ends. It used to be missing, and
//! that was a hang rather than a gap: [`request_sdk_tool_permission`] has the
//! stream-closed branch and cites `:254-260`, but nothing produced it, so at
//! EOF the `Sender` parked in `SdkControlBridge::pending` kept its channel open
//! and `recv()` waited forever where CC terminates the request.
//!
//! NOT ported, and each an explicit seam rather than a decision: the rest of the
//! class shell, `prependUserMessage` (`:204-213`),
//! `getPendingPermissionRequests` (`:263-267`),
//! `setUnexpectedResponseCallback` (`:269-273`), `injectControlResponse`
//! (`:283-309`), the `setOnControlRequestSent`/`setOnControlRequestResolved`
//! bridge callbacks (`:316-331`), the `resolvedToolUseIds` duplicate-response
//! guard (`:155`/`:176-187`), `sendMcpMessage` (`:758-773`),
//! `createSandboxAskCallback` + `SANDBOX_NETWORK_ACCESS_TOOL_NAME`
//! (`:62`/`:731-753`), and the race's `onPermissionPrompt` /
//! `buildRequiresActionDetails` / `notifySessionStateChanged('running')`
//! session-state leg (`:534`, `:587-589`, `:650-656`) — the
//! `RequiresActionDetails` type it would feed already exists in
//! `crate::utils::session_state`, unwired.

use crate::cli::print::{ControlResponseInput, json_line};

/// Applies one `update_environment_variables` stdin message to the process
/// env carrier. Used by the bridge session runner for auth-token refresh
/// (`CLAUDE_CODE_SESSION_ACCESS_TOKEN`), which must be readable by the REPL
/// process itself — `getSessionIngressAuthToken()` re-reads it per
/// `refreshHeaders` — not just by child Bash commands.
///
/// Maps to: CC `cli/structuredIO.ts:348-360` (`processLine`'s
/// `update_environment_variables` arm). The stdin loop lives in
/// `crate::cli::print` (MODULE_MAP row 64); it delegates the application here
/// so the env write stays with the StructuredIO owner.
pub(super) fn apply_environment_update(variables: &indexmap::IndexMap<String, String>) {
    let entries = crate::utils::process_env::ecmascript_object_entries(variables.iter());
    let mut update = crate::utils::process_env::begin_update();
    update.apply(entries.iter().map(|(key, value)| (*key, value.as_str())));
    update.commit();
    // Log keys only; values may carry credentials (CC `:357-359`).
    tracing::debug!(
        "[structuredIO] applied update_environment_variables: {}",
        entries
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

pub(super) type PendingControlResponses = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<String, async_channel::Sender<ControlResponseInput>>,
    >,
>;

#[derive(Clone)]
pub(super) struct SdkControlBridge {
    pub(super) pending: PendingControlResponses,
    /// CC `StructuredIO.inputClosed` (`cli/structuredIO.ts:144`). Exactly two
    /// sites touch it upstream — `read()`'s close tail sets it (`:254`) and
    /// `sendRequest` reads it (`:480`) — and the port has the same two, as
    /// [`SdkControlBridge::close_input`] and
    /// [`SdkControlBridge::register_pending`]:
    ///
    /// ```text
    /// $ ast-grep --lang ts -p 'this.inputClosed' src/
    /// src/cli/structuredIO.ts:254:    this.inputClosed = true
    /// src/cli/structuredIO.ts:480:    if (this.inputClosed) {
    /// ```
    ///
    /// (`cli/print.ts` has an `inputClosed` local of its own at `:1017`; that
    /// is `runHeadlessStreaming` loop state, not this flag.)
    input_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The one sink every outbound SDK control message leaves through. What
    /// this DOES reproduce of CC `StructuredIO.outbound`
    /// (`cli/structuredIO.ts:160-162`) is the no-exceptions rule INSIDE the
    /// class: every control request is built by `sendRequest`, whose only
    /// write is `this.outbound.enqueue(message)` (`:486`), and its abort
    /// listener enqueues the cancel to the same place (`:491`). So
    /// `request_sdk_tool_permission`, `request_sdk_hook_callback` and
    /// `request_sdk_elicitation` all call `emit`, and none reaches `json_line`
    /// on its own. Tests install a capture here, which is also the only reason
    /// the three legs are observable at all. (CC's own one exception,
    /// `injectControlResponse`'s direct `void this.write(...)` at `:291`, is
    /// on the bridge path this port does not have — already booked in the
    /// module doc's not-ported list.)
    ///
    /// What it does NOT reproduce is the queue. CC buffers into a
    /// `Stream<StdoutMessage>` that `cli/print.ts:864-886` drains in a single
    /// loop, because there a direct `write()` (`:465-467`) emits bytes while
    /// earlier-`enqueue`d stream events still sit undrained — exactly the
    /// hazard `:160-162` names ("Prevents control_request from overtaking
    /// queued stream_events"). `emit` writes through `json_line`
    /// synchronously, so no undrained backlog exists to overtake: write order
    /// is call order.
    ///
    /// That is why the queue is not ported rather than merely missing. The two
    /// writers a queue would have to serialize — this `emit` and
    /// `QueryEngine::output_sink` — are both driven by the single
    /// `while let Ok(event) = handle.events.recv().await` loop in
    /// `query_engine::run_query`, as sibling arms of one `match`
    /// (`QueryEvent::Message`/`Stream` → `emit_output`,
    /// `QueryEvent::PermissionRequest` → `request_sdk_tool_permission`). One
    /// `async_channel` drained in the actor's emission order already gives
    /// them CC's property, so a FIFO in front of `json_line` would add a
    /// flush-on-exit hazard and no ordering. Genuinely unreproduced, and
    /// out of this file: the queue as a shutdown-ordered buffer, and the
    /// `runHeadless` accounting that rides the same drain
    /// (`lastMessage`/`messages`, `cli/print.ts:892-914`).
    emit: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>,
}

impl Default for SdkControlBridge {
    fn default() -> Self {
        SdkControlBridge {
            pending: PendingControlResponses::default(),
            input_closed: std::sync::Arc::default(),
            emit: std::sync::Arc::new(json_line),
        }
    }
}

impl SdkControlBridge {
    /// Maps to: CC `cli/structuredIO.ts:480-482` + `:512-523` — the head and
    /// the tail of `sendRequest`. CC refuses to send once `read()` has closed
    /// the input, and only PAST that guard does it enqueue the outbound message
    /// (`:486`) and park the request in `pendingRequests` (`:512-523`):
    ///
    /// ```text
    /// if (this.inputClosed) {
    ///   throw new Error('Stream closed')
    /// }
    /// ```
    ///
    /// `false` is that throw. Each leg then produces what CC's caller produces
    /// for it, and — like CC — sends nothing, because the guard sits ahead of
    /// the enqueue.
    ///
    /// Without this the close tail would still leave a hang, just a narrower
    /// one: `run_stream_json` awaits a whole `ask()` turn, so a query can raise
    /// a permission request AFTER the reader thread has hit EOF and gone away,
    /// and that request would park on a map with no remaining producer.
    ///
    /// The flag is read while the map lock is held, and [`Self::close_input`]
    /// sets it while holding the same lock, so a request racing EOF either
    /// registers before the drain (and is freed by it) or observes the flag. It
    /// cannot land in an already-drained map.
    fn register_pending(
        &self,
        request_id: &str,
        sender: async_channel::Sender<ControlResponseInput>,
    ) -> bool {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.input_closed.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        pending.insert(request_id.to_string(), sender);
        true
    }

    /// Maps to: CC `cli/structuredIO.ts:254-260` — the tail of `read()`, which
    /// runs once the stdin stream is exhausted:
    ///
    /// ```text
    /// this.inputClosed = true
    /// for (const request of this.pendingRequests.values()) {
    ///   // Reject all pending requests if the input stream
    ///   request.reject(
    ///     new Error('Tool permission stream closed before response received'),
    ///   )
    /// }
    /// ```
    ///
    /// Dropping the parked `Sender` IS that rejection here: the `recv()` on the
    /// other side stops waiting and returns `Err`, which is the arm every leg
    /// already routes to CC's own catch — the `:254-260` stream-closed deny for
    /// [`request_sdk_tool_permission`], `{}` for [`request_sdk_hook_callback`]
    /// (CC `:682-686`) and `{action:'cancel'}` for [`request_sdk_elicitation`]
    /// (CC `:718-720`). Those three are the only registrants — the query
    ///
    /// ```text
    /// ast-grep --lang rust -p '$B.pending.lock().unwrap_or_else($C).insert($$$ARGS)' src/
    /// ```
    ///
    /// matches exactly three chains, one at the head of each of those
    /// functions, so draining the map frees every parked request.
    ///
    /// Three of CC's five, at that: `ast-grep --lang ts -p 'this.sendRequest'
    /// src/cli/` lists `:590` (`createCanUseTool`), `:671`
    /// (`createHookCallback`), `:704` (`handleElicitation`), `:737`
    /// (`createSandboxAskCallback`) and `:762` (`sendMcpMessage`). The last two
    /// are unported seams (module doc). When either lands it must go through
    /// [`Self::register_pending`] rather than touching `pending` directly, or
    /// it reintroduces exactly the parked-forever case this method exists to
    /// end.
    pub(super) fn close_input(&self) {
        let parked = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.input_closed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            std::mem::take(&mut *pending)
        };
        // Every `Sender` goes out of scope here, outside the lock: the channels
        // close and every parked `recv()` errors.
        drop(parked);
    }
}

/// Maps to: CC `structuredIO.createHookCallback(callbackId, timeout)`
/// (structuredIO.ts:661-689) — a HookCallback whose callback sends a
/// `hook_callback` control request and validates the consumer's response
/// against `hook_json_output_schema`; any failure resolves to `{}` (CC's
/// catch logs and returns the empty output).
pub(super) fn create_hook_callback(
    bridge: &SdkControlBridge,
    callback_id: String,
    timeout: Option<u64>,
) -> crate::schemas::hooks::HookCallback {
    let bridge = bridge.clone();
    crate::schemas::hooks::HookCallback {
        timeout,
        callback: std::sync::Arc::new(move |input, tool_use_id| {
            let bridge = bridge.clone();
            let callback_id = callback_id.clone();
            Box::pin(async move {
                request_sdk_hook_callback(&bridge, &callback_id, input, tool_use_id).await
            })
        }),
    }
}

async fn request_sdk_hook_callback(
    bridge: &SdkControlBridge,
    callback_id: &str,
    input: serde_json::Value,
    tool_use_id: Option<String>,
) -> serde_json::Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = async_channel::bounded(1);
    if !bridge.register_pending(&request_id, sender) {
        // CC `:480-482` — `sendRequest` throws `Stream closed` before it writes
        // anything, and `createHookCallback`'s catch (`:682-686`) logs and
        // returns the empty output.
        return serde_json::json!({});
    }
    let mut request = serde_json::json!({
        "subtype": "hook_callback",
        "callback_id": callback_id,
        "input": input,
    });
    if let Some(tool_use_id) = tool_use_id {
        request["tool_use_id"] = serde_json::json!(tool_use_id);
    }
    // CC `:669-680` reaches the wire through `sendRequest`, whose only write is
    // `this.outbound.enqueue(message)` (`:486`) — one sink, no exceptions.
    (bridge.emit)(serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    }));

    let Ok(response) = receiver.recv().await else {
        return serde_json::json!({});
    };
    if response.subtype != "success" {
        return serde_json::json!({});
    }
    let Some(result) = response.response else {
        return serde_json::json!({});
    };
    match crate::utils::zod::safe_parse(crate::types::hooks::hook_json_output_schema(), &result) {
        Ok(data) => data,
        Err(_) => serde_json::json!({}),
    }
}

/// Maps to: CC `structuredIO.handleElicitation(...)` (structuredIO.ts:694-721)
/// as consumed by the QueryEngine leg (print.ts:2189-2198) — sends an
/// `elicitation` control request and awaits the SDK consumer's response. Any
/// failure (closed channel, error subtype, malformed action) resolves to
/// `{action: 'cancel'}`, matching CC's catch. The QueryEngine leg forwards
/// message/mode/url/elicitation_id and drops the form's requestedSchema, as
/// CC's does.
pub(super) async fn request_sdk_elicitation(
    bridge: &SdkControlBridge,
    server_name: &str,
    params: crate::services::mcp::elicitation_handler::ElicitationRequestParams,
) -> crate::services::mcp::elicitation_handler::ElicitationResult {
    use crate::services::mcp::elicitation_handler::{
        ElicitationAction, ElicitationRequestParams, ElicitationResult,
    };

    let request_id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = async_channel::bounded(1);
    if !bridge.register_pending(&request_id, sender) {
        // CC `:480-482` — `sendRequest` throws `Stream closed` before it writes
        // anything, and `handleElicitation`'s catch (`:718-720`) cancels.
        return ElicitationResult::new(ElicitationAction::Cancel);
    }
    let mut request = serde_json::json!({
        "subtype": "elicitation",
        "mcp_server_name": server_name,
    });
    match params {
        ElicitationRequestParams::Form { message, .. } => {
            request["message"] = serde_json::json!(message);
            request["mode"] = serde_json::json!("form");
        }
        ElicitationRequestParams::Url {
            message,
            url,
            elicitation_id,
        } => {
            request["message"] = serde_json::json!(message);
            request["mode"] = serde_json::json!("url");
            request["url"] = serde_json::json!(url);
            if let Some(elicitation_id) = elicitation_id {
                request["elicitation_id"] = serde_json::json!(elicitation_id);
            }
        }
    }
    // CC `:704-716` reaches the wire through `sendRequest`, whose only write is
    // `this.outbound.enqueue(message)` (`:486`) — one sink, no exceptions.
    (bridge.emit)(serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    }));

    let Ok(response) = receiver.recv().await else {
        return ElicitationResult::new(ElicitationAction::Cancel);
    };
    if response.subtype != "success" {
        return ElicitationResult::new(ElicitationAction::Cancel);
    }
    let Some(result) = response.response else {
        return ElicitationResult::new(ElicitationAction::Cancel);
    };
    // CC validates against SDKControlElicitationResponseSchema — action is a
    // required enum; a mismatch throws and lands in the cancel catch.
    let action = match result.get("action").and_then(serde_json::Value::as_str) {
        Some("accept") => ElicitationAction::Accept,
        Some("decline") => ElicitationAction::Decline,
        Some("cancel") => ElicitationAction::Cancel,
        _ => return ElicitationResult::new(ElicitationAction::Cancel),
    };
    ElicitationResult {
        action,
        content: result.get("content").filter(|c| c.is_object()).cloned(),
    }
}

/// Maps to: CC `cli/structuredIO.ts:590-602` — the exact `can_use_tool`
/// control-request payload:
///
/// ```text
/// { subtype: 'can_use_tool', tool_name, input,
///   permission_suggestions: mainPermissionResult.suggestions,
///   blocked_path: mainPermissionResult.blockedPath,
///   decision_reason: serializeDecisionReason(mainPermissionResult.decisionReason),
///   tool_use_id: toolUseID, agent_id: toolUseContext.agentId }
/// ```
///
/// Eight keys and no more. It has never carried a `title` (removed in #123 with
/// the fabricated `PermissionRequest.title`) nor a `description` (removed in
/// #132: `tool.description(...)` is a DIALOG string and the SDK host renders its
/// own prompt).
///
/// `undefined` values are dropped by `JSON.stringify`, so the four optional keys
/// are omitted rather than sent as `null`. One flattening remains: CC can send
/// `permission_suggestions: []` when a tool returned an empty array, while this
/// port's `PermissionRequest.suggestions` cannot distinguish `[]` from absent
/// and omits both.
fn can_use_tool_control_request(
    request: &crate::types::permissions::PermissionRequest,
    agent_id: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "subtype": "can_use_tool",
        "tool_name": request.tool_name,
        "input": request.input,
        "tool_use_id": request.tool_use_id,
    });
    if !request.suggestions.is_empty() {
        payload["permission_suggestions"] = serde_json::Value::Array(
            crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
                &request.suggestions,
            ),
        );
    }
    if let Some(blocked_path) = request.blocked_path.as_deref() {
        payload["blocked_path"] = serde_json::Value::String(blocked_path.to_string());
    }
    if let Some(decision_reason) = serialize_decision_reason(request.decision_reason.as_ref()) {
        payload["decision_reason"] = serde_json::Value::String(decision_reason);
    }
    if let Some(agent_id) = agent_id {
        payload["agent_id"] = serde_json::Value::String(agent_id.to_string());
    }
    payload
}

/// Maps to: CC `cli/structuredIO.ts:64-91#serializeDecisionReason` — the
/// projection that turns a `PermissionDecisionReason` into the flat
/// `decision_reason` string the `can_use_tool` control request carries.
///
/// It is deliberately lossy, and the losses are the contract: `rule`, `mode`,
/// `subcommandResults` and `permissionPromptTool` return `undefined` (they carry
/// no prose), while `hook` / `asyncAgent` / `sandboxOverride` / `workingDir` /
/// `safetyCheck` / `other` return their own `reason`. `hook.reason` is optional
/// upstream (`types/permissions.ts:293`), so it can still be `undefined`.
///
/// The `classifier` arm sits ahead of the switch behind
/// `feature('BASH_CLASSIFIER') || feature('TRANSCRIPT_CLASSIFIER')`; with both
/// off the switch has no `classifier` case, so control falls off the end and the
/// function returns `undefined`. The port keeps the guard rather than its
/// outcome (`source-of-truth.md` § build-time constants).
fn serialize_decision_reason(
    reason: Option<&crate::utils::permissions::permission_result::PermissionDecisionReason>,
) -> Option<String> {
    use crate::utils::permissions::permission_result::{
        PermissionDecisionReason, SandboxOverrideReason,
    };

    let reason = reason?;
    if matches!(reason, PermissionDecisionReason::Classifier { .. })
        && (crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::BashClassifier,
        ) || crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
        ))
    {
        if let PermissionDecisionReason::Classifier { reason, .. } = reason {
            return Some(reason.clone());
        }
    }
    match reason {
        PermissionDecisionReason::Rule { .. }
        | PermissionDecisionReason::Mode { .. }
        | PermissionDecisionReason::SubcommandResults { .. }
        | PermissionDecisionReason::PermissionPromptTool { .. }
        // No `classifier` case in CC's switch: an unguarded classifier reason
        // reaches the end of the function and yields `undefined`.
        | PermissionDecisionReason::Classifier { .. } => None,
        PermissionDecisionReason::Hook { reason, .. } => reason.clone(),
        PermissionDecisionReason::AsyncAgent { reason }
        | PermissionDecisionReason::WorkingDir { reason }
        | PermissionDecisionReason::SafetyCheck { reason, .. }
        | PermissionDecisionReason::Other { reason } => Some(reason.clone()),
        PermissionDecisionReason::SandboxOverride { reason } => Some(
            match reason {
                SandboxOverrideReason::ExcludedCommand => "excludedCommand",
                SandboxOverrideReason::DangerouslyDisableSandbox => "dangerouslyDisableSandbox",
            }
            .to_string(),
        ),
    }
}

/// Maps to: CC `cli/structuredIO.ts:533-659` — the `can_use_tool` control
/// request raced against the PermissionRequest hooks, and the
/// `PermissionToolOutput` response it awaits.
///
/// `agent_id` is CC's `toolUseContext.agentId` (`Tool.ts:245`, "Only set for
/// subagents"); this port threads it from the same `ToolUseContext` the abort
/// controller comes from, because the resolver closure has no context of its own.
///
/// Race shape (CC `:561-638`): the SDK prompt is sent immediately and the
/// hook evaluation starts in parallel. The hook leg always resolves — CC's
/// "The hook promise always resolves (never rejects), returning undefined if
/// no hook made a decision" (`:608-610`); hook command failures surface as
/// decision-less results inside `executePermissionRequestHooks`, never as a
/// race rejection. Hook wins WITH a decision → the pending SDK request is
/// aborted (`sdkPromise.catch(() => {}); hookAbortController.abort()`,
/// `:613-619`), which enqueues the outbound `control_cancel_request` and
/// rejects the local promise (`:490-504`). Hook wins WITHOUT a decision →
/// keep awaiting the SDK prompt (`:620-628`). SDK wins → its result is used
/// while the hooks keep running in the background, result ignored (`:631-638`).
pub(super) async fn request_sdk_tool_permission(
    bridge: &SdkControlBridge,
    request: &crate::types::permissions::PermissionRequest,
    abort: &crate::tool::AbortController,
    agent_id: Option<&str>,
    app_store: &crate::tool::AppStoreRef,
) -> crate::types::permissions::PermissionPromptResponse {
    let request_id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = async_channel::bounded(1);
    if !bridge.register_pending(&request_id, sender) {
        // CC `:480-482` — `sendRequest` throws `new Error('Stream closed')`
        // BEFORE `outbound.enqueue` (`:486`) and before the pending entry
        // exists (`:512`), so a request raised after stdin EOF sends nothing
        // and resolves at once; `createCanUseTool`'s catch (`:639-649`) turns
        // the throw into the synthetic deny.
        //
        // One outcome-identical divergence: CC starts `hookPromise` BEFORE
        // `sendRequest` (`:577-590`), so a closed stream still leaves the hook
        // running in the background and its side effects (a persisted "always
        // allow") still land. This port emits before it spawns the hook, so
        // the guard sits ahead of the spawn and no hook runs. The DECISION is
        // the same either way: a `sendRequest` that throws settles CC's race
        // many ticks before any hook can, so the hook's decision is discarded
        // upstream too.
        return normalized_sdk_permission_response(
            sdk_permission_request_failed(request, "Error: Stream closed"),
            request,
            abort,
            app_store,
        );
    }
    (bridge.emit)(serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": can_use_tool_control_request(request, agent_id),
    }));

    // CC `:576-583` — start the hook evaluation in the background. Spawned
    // (not merely selected) so an SDK win leaves it running to completion
    // exactly like CC's un-awaited promise: the losing hook's side effects
    // (persisted "always allow" updates) still land, its decision is ignored.
    let hook_future = {
        let request = request.clone();
        let abort = abort.clone();
        let app_store = app_store.clone();
        // CC reaches the session id through the same `toolUseContext` it hands
        // `executePermissionRequestHooksForSDK` (`:580`); this port only has
        // the `agentId` projection of it, which is the half that matters —
        // `executeHooks` reads `toolUseContext?.agentId ?? getSessionId()`
        // (`hooks.ts:2003`).
        let agent_id = agent_id.map(str::to_string);
        tokio::spawn(async move {
            execute_permission_request_hooks_for_sdk(
                &request,
                &abort,
                agent_id.as_deref(),
                &app_store,
            )
            .await
        })
    };
    let mut hook_task = Some(hook_future);

    // Enqueue the outbound cancel and drop the local pending entry without
    // waiting for host acknowledgement (CC `:490-504`).
    let cancel_sdk_request = |bridge: &SdkControlBridge, request_id: &str| {
        (bridge.emit)(serde_json::json!({
            "type": "control_cancel_request",
            "request_id": request_id,
        }));
        bridge
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(request_id);
    };

    let response = loop {
        if abort.is_aborted() {
            // CC `structuredIO.ts:490-504` — the abort listener first enqueues
            // an outbound `control_cancel_request` (so the host's canUseTool
            // callback is released instead of hanging), then rejects the
            // pending request with `AbortError` (stringifies as "AbortError",
            // `utils/errors.ts:12-17`); `createCanUseTool`'s catch turns it
            // into a deny through the canonical normalizer. This used to
            // return `PermissionPromptChoice::Cancel`, a variant CC's answer
            // space does not have — and used to skip the cancel message.
            cancel_sdk_request(bridge, &request_id);
            return normalized_sdk_permission_response(
                sdk_permission_request_failed(request, "AbortError"),
                request,
                abort,
                app_store,
            );
        }
        tokio::select! {
            // CC `Promise.race([hookPromise, sdkPromise])` lists the hook
            // first; `biased` keeps that ordering when both are ready.
            biased;
            hook = async { hook_task.as_mut().expect("guarded by if").await }, if hook_task.is_some() => {
                // A JoinError (panicked hook task) maps to CC's always-resolves
                // contract: no decision, keep awaiting the SDK prompt.
                match hook.ok().flatten() {
                    Some(decision) => {
                        // CC `:613-619` — hook decided: suppress and abort the
                        // pending SDK request, return the hook decision.
                        cancel_sdk_request(bridge, &request_id);
                        return decision;
                    }
                    None => {
                        // CC `:620-628` — hook passed through: wait for the
                        // SDK prompt response.
                        hook_task = None;
                    }
                }
            }
            response = receiver.recv() => break response.ok(),
            _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
        }
    };
    let Some(response) = response else {
        // CC `structuredIO.ts:254-260` — closing the input stream rejects every
        // pending request with `Tool permission stream closed before response
        // received`; the catch wraps it as `Error: …`.
        //
        // The producer is [`SdkControlBridge::close_input`], which the
        // `sdk-stdin-reader` thread calls when stdin ends: it drops this
        // request's `Sender`, so `recv()` errors instead of parking forever.
        return normalized_sdk_permission_response(
            sdk_permission_request_failed(
                request,
                "Error: Tool permission stream closed before response received",
            ),
            request,
            abort,
            app_store,
        );
    };
    permission_prompt_response_from_control(response, request, abort, app_store)
}

/// Maps to: CC `cli/structuredIO.ts:783-859#executePermissionRequestHooksForSDK`
/// — execute PermissionRequest hooks and return a decision if one is made;
/// `None` when no hook decided (or no hooks are registered).
///
/// The first allow/deny `permissionRequestResult` wins (`:808-813` — the
/// decision object, NOT the flattened permissionBehavior). An allow applies
/// `updatedPermissions` with `persistPermissionUpdates` FIRST and the
/// `setAppState` projection after (`:819-831`), the #159 fail-closed order;
/// a persist failure takes the whole decision to `createCanUseTool`'s catch
/// (`:639-649`), i.e. the synthetic `Tool permission request failed: …` deny.
/// The allow decision carries `finalInput = decision.updatedInput || input`
/// (`:816`) and `decisionReason {type:'hook', hookName:'PermissionRequest'}`
/// (`:838-841`); the deny carries
/// `decision.message || 'Permission denied by PermissionRequest hook'`
/// (`:843-853`, JS `||`: an empty message takes the fallback).
///
/// Hook SET (`:798-806` → `hooks.ts:4182-4191#executeHooks` →
/// `:2003-2010#getMatchingHooks` → `:1491-1565#getHooksConfig`): CC assembles
/// FOUR sources, in this order — the settings snapshot
/// (`getHooksConfigFromSnapshot()?.[hookEvent]`), the registered SDK/plugin
/// hooks (`getRegisteredHooks()`, plugin entries filtered out under
/// `shouldAllowManagedHooksOnly()`), then, unless managed-only, the current
/// session's `appState.sessionHooks` and its function hooks. The session id is
/// `toolUseContext?.agentId ?? getSessionId()` (`:2003`), so a subagent's own
/// registrations are what it reads.
///
/// `load_hooks_config()` covers only the first two ("Session-derived hooks
/// remain scoped and merged by their callers", its own doc comment), so this
/// race merged nothing an agent registered at runtime through
/// `registerFrontmatterHooks` (CC `runAgent.ts:568`, ported as
/// `run_agent.rs#register_agent_frontmatter_hooks_for_run`) — the same gap
/// `tool_execution.rs#load_tool_hooks_config_and_env`,
/// `run_agent.rs#subagent_hook_config_and_env` and
/// `process_user_input/mod.rs` already close with the shared
/// `session_hooks::merge_session_hooks_into_config`, including its
/// `allow_managed_hooks_only` gate (CC `:1515` + `:1541`). Function hooks stay
/// out: `merge_session_hooks_into_config` flattens command hooks only, and this
/// port has no `FunctionHookMatcher` executor yet.
async fn execute_permission_request_hooks_for_sdk(
    request: &crate::types::permissions::PermissionRequest,
    abort: &crate::tool::AbortController,
    agent_id: Option<&str>,
    app_store: &crate::tool::AppStoreRef,
) -> Option<crate::types::permissions::PermissionPromptResponse> {
    use crate::types::permissions::{
        PermissionDecisionReason, PermissionPromptChoice, PermissionPromptResponse,
    };

    let loaded = crate::services::hooks::load_hooks_config();
    let mut config = loaded.config;
    if !loaded.allow_managed_hooks_only {
        let session_id = agent_id
            .map(str::to_string)
            .unwrap_or_else(crate::bootstrap::state::get_session_id);
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
    if !has_hooks {
        return None;
    }
    let results = crate::services::hooks::permission_request::execute_permission_request_hooks(
        &config,
        request,
        Vec::new(),
        Some(abort),
    )
    .await;
    for result in &results {
        let Some(decision) = result.permission_request_result.as_ref() else {
            continue;
        };
        match decision {
            crate::types::hooks::PermissionRequestResult::Allow {
                updated_input,
                updated_permissions,
            } => {
                // CC `:816` — `decision.updatedInput || input`.
                let final_input = updated_input
                    .clone()
                    .unwrap_or_else(|| request.input.clone());
                // CC `:818-831` — persist FIRST, project after (#159
                // fail-closed: never project what failed to persist).
                if !updated_permissions.is_empty() {
                    match crate::utils::permissions::permission_update::persist_permission_updates(
                        updated_permissions,
                    ) {
                        Ok(()) => app_store.set_app_state(|state| {
                            state.tool_permission_context = std::sync::Arc::new(
                                crate::utils::permissions::permission_update::apply_permission_updates(
                                    &state.tool_permission_context,
                                    updated_permissions,
                                ),
                            );
                        }),
                        Err(error) => {
                            // CC: `persistPermissionUpdates` throwing rejects
                            // the hook promise, and `createCanUseTool`'s catch
                            // (`:639-649`) feeds the synthetic deny through
                            // the canonical normalizer.
                            return Some(normalized_sdk_permission_response(
                                sdk_permission_request_failed(
                                    request,
                                    &format!("Error: {error}"),
                                ),
                                request,
                                abort,
                                app_store,
                            ));
                        }
                    }
                }
                return Some(
                    PermissionPromptResponse::allow_once_with_input(final_input)
                        // CC's decision has no updatedPermissions field
                        // (`:834-842`); the response still carries them so the
                        // query actor projects the in-session permission
                        // context (this port bridges two containers — see
                        // `normalized_sdk_permission_response`).
                        .with_permission_updates(updated_permissions.clone())
                        .with_decision_reason(Some(PermissionDecisionReason::Hook {
                            hook_name: "PermissionRequest".to_string(),
                            hook_source: None,
                            reason: None,
                        })),
                );
            }
            crate::types::hooks::PermissionRequestResult::Deny { message, .. } => {
                return Some(
                    PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                        .with_decision_message(
                            message
                                .clone()
                                .filter(|message| !message.is_empty())
                                .unwrap_or_else(|| {
                                    "Permission denied by PermissionRequest hook".to_string()
                                }),
                        )
                        .with_decision_reason(Some(PermissionDecisionReason::Hook {
                            hook_name: "PermissionRequest".to_string(),
                            hook_source: None,
                            reason: None,
                        })),
                );
            }
        }
    }
    None
}

/// Maps to: CC `cli/structuredIO.ts:611-649` — every control_response outcome
/// funnels through the canonical `PermissionPromptToolResultSchema` pair.
///
/// A success payload is parsed by `outputSchema` and normalized by
/// `permissionPromptToolResultToPermissionDecision`. Every failure — error
/// subtype (`:411-413` `request.reject(new Error(...))`), missing payload, or
/// schema mismatch (`:417-421` `request.schema.parse` throwing) — lands in
/// `createCanUseTool`'s catch (`:639-649`), which feeds a synthetic
/// `{behavior:'deny', message:`Tool permission request failed: ${error}`}`
/// through the SAME normalizer.
fn permission_prompt_response_from_control(
    response: ControlResponseInput,
    request: &crate::types::permissions::PermissionRequest,
    abort: &crate::tool::AbortController,
    app_store: &crate::tool::AppStoreRef,
) -> crate::types::permissions::PermissionPromptResponse {
    let output = match control_permission_tool_output(response) {
        Ok(output) => output,
        Err(detail) => sdk_permission_request_failed(request, &detail),
    };
    normalized_sdk_permission_response(output, request, abort, app_store)
}

/// The parse phase of CC `structuredIO.ts`' control_response handling.
/// `Err` is a rejection reason as `createCanUseTool`'s catch would stringify
/// it (`${error}`); the caller wraps it in the catch's synthetic deny.
fn control_permission_tool_output(
    response: ControlResponseInput,
) -> Result<
    crate::utils::permissions::permission_prompt_tool_result_schema::PermissionPromptToolOutput,
    String,
> {
    if response.subtype != "success" {
        // CC `:411-413`: `request.reject(new Error(message.response.error))`.
        return Err(format!(
            "Error: {}",
            response
                .error
                .unwrap_or_else(|| "SDK permission request failed".to_string())
        ));
    }
    let Some(result) = response.response else {
        // CC: `request.schema.parse(undefined)` throws.
        return Err("Error: SDK permission response had no result".to_string());
    };
    // CC: `permissionToolOutputSchema()` — a mismatch throws a ZodError. The
    // Rust parser has no issue list to stringify; the prefix contract
    // (`Tool permission request failed: `) is what the model-facing text keys on.
    crate::utils::permissions::permission_prompt_tool_result_schema::permission_prompt_tool_output_from_official_json(&result)
        .ok_or_else(|| {
            "Error: SDK permission response did not match the permission tool output schema"
                .to_string()
        })
}

/// CC `cli/structuredIO.ts:639-649` — the catch's synthetic deny, fed through
/// the canonical normalizer like any SDK deny.
fn sdk_permission_request_failed(
    request: &crate::types::permissions::PermissionRequest,
    detail: &str,
) -> crate::utils::permissions::permission_prompt_tool_result_schema::PermissionPromptToolOutput {
    crate::utils::permissions::permission_prompt_tool_result_schema::PermissionPromptToolOutput::Deny {
        message: format!("Tool permission request failed: {detail}"),
        interrupt: false,
        tool_use_id: Some(request.tool_use_id.clone()),
        decision_classification: None,
    }
}

/// The normalize phase: CC `permissionPromptToolResultToPermissionDecision`
/// plus its side effects, projected onto the resolver's transport type.
///
/// Persist-vs-state ordering (#170): TWO CC sites govern TWO different Rust
/// paths, deliberately NOT unified —
/// - THIS path (SDK `can_use_tool` normalizer) follows
///   `PermissionPromptToolResultSchema.ts:95-106`: `setAppState` projects the
///   allow's `updatedPermissions` into the live AppState FIRST (`:98-104`),
///   `persistPermissionUpdates` runs AFTER (`:105`); a persist throw lands in
///   `createCanUseTool`'s catch (`cli/structuredIO.ts:639-649`), which turns
///   the WHOLE decision into a synthetic deny while the state projection
///   stands.
/// - The headless PermissionRequest-hook path
///   (`utils/permissions/permissions.rs#resolve_headless_ask_owned`) follows
///   `utils/permissions/permissions.ts:425-433` — `persistPermissionUpdates`
///   FIRST (`:426`), `setAppState` after (`:427-433`); its SDK-side sibling
///   `cli/structuredIO.ts:819-831` has the same order. That is the #159
///   fail-closed rule: project only after a successful persist.
fn normalized_sdk_permission_response(
    output: crate::utils::permissions::permission_prompt_tool_result_schema::PermissionPromptToolOutput,
    request: &crate::types::permissions::PermissionRequest,
    abort: &crate::tool::AbortController,
    app_store: &crate::tool::AppStoreRef,
) -> crate::types::permissions::PermissionPromptResponse {
    use crate::types::permissions::{
        PermissionDecision, PermissionPromptChoice, PermissionPromptResponse,
    };

    // The normalizer only reads `tool.name` (`PermissionPromptToolResultSchema.ts:93`).
    // CC's closure holds the real Tool object; this resolver boundary only has
    // the wire name, so a name-only Tool is behaviorally identical here.
    let tool = crate::types::tools::Tool {
        name: request.tool_name.clone(),
        ..Default::default()
    };
    let normalization = crate::utils::permissions::permission_prompt_tool_result_schema::
        permission_prompt_tool_result_to_permission_decision(output, &tool, &request.input);
    // CC `:117-122` — a deny with `interrupt` aborts the tool-use controller.
    if normalization.abort_requested {
        abort.abort();
    }
    // CC `:96-106` — an allow's `updatedPermissions` is projected into the
    // live AppState FIRST (`:98-104` `toolUseContext.setAppState`), then
    // persisted (`:105`). The response still carries the updates so the query
    // actor's `apply_prompt_response` projects them into its in-session
    // permission context (CC has one container; this port bridges two).
    if !normalization.updated_permissions.is_empty() {
        app_store.set_app_state(|state| {
            state.tool_permission_context = std::sync::Arc::new(
                crate::utils::permissions::permission_update::apply_permission_updates(
                    &state.tool_permission_context,
                    &normalization.updated_permissions,
                ),
            );
        });
        if let Err(error) = crate::utils::permissions::permission_update::persist_permission_updates(
            &normalization.updated_permissions,
        ) {
            // CC `persistPermissionUpdates` throwing takes the whole allow to
            // `createCanUseTool`'s catch (`cli/structuredIO.ts:639-649`),
            // which feeds a synthetic `Tool permission request failed: …`
            // deny back through this same normalizer. The state projection
            // above is NOT rolled back — CC's `setAppState` already ran.
            return normalized_sdk_permission_response(
                sdk_permission_request_failed(request, &format!("Error: {error}")),
                request,
                abort,
                app_store,
            );
        }
    }
    match normalization.decision {
        PermissionDecision::Allow {
            updated_input,
            decision_reason,
            tool_use_id,
            ..
        } => PermissionPromptResponse::allow_once_with_input(
            updated_input.unwrap_or_else(|| request.input.clone()),
        )
        .with_permission_updates(normalization.updated_permissions)
        .with_decision_reason(decision_reason)
        .with_tool_use_id(tool_use_id),
        PermissionDecision::Deny {
            message,
            decision_reason,
            tool_use_id,
        } => {
            // CC returns the deny's raw `message` as the decision that resolves
            // `canUseTool` (`toolExecution.ts:1023`) — a SYSTEM decision
            // message. Routing it through `feedback` would prepend
            // REJECT_MESSAGE_WITH_REASON_PREFIX, which CC does not do here.
            PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                .with_decision_message(message)
                .with_decision_reason(Some(decision_reason))
                .with_tool_use_id(tool_use_id)
        }
        // The normalizer only produces allow/deny (CC returns the schema's
        // union, which has no ask arm). Deny is the safe projection if that
        // ever changes.
        PermissionDecision::Ask { .. } => {
            PermissionPromptResponse::new(PermissionPromptChoice::Deny)
                .with_decision_message(request.message.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// CC `cli/structuredIO.ts:348-360` enumerates the variables object and
    /// performs its assignments in that official own-key order.
    #[test]
    fn environment_updates_match_official_structured_io_process_env_order() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let keys = ["COMETIX_STRUCTURED_ENV_B", "COMETIX_STRUCTURED_ENV_A"];
        let _guards = keys.map(EnvVarGuard::unset);
        let _numeric = EnvVarGuard::unset("1");
        let variables = indexmap::IndexMap::from([
            (keys[0].to_string(), "second".to_string()),
            (keys[1].to_string(), "first".to_string()),
        ]);

        apply_environment_update(&variables);

        let actual = crate::utils::process_env::snapshot()
            .iter()
            .filter_map(|(key, value)| {
                let key = key.to_str()?;
                keys.contains(&key)
                    .then(|| (key.to_string(), value.to_string_lossy().into_owned()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                (keys[0].to_string(), "second".to_string()),
                (keys[1].to_string(), "first".to_string()),
            ]
        );

        // Object.entries enumerates the exact integer index before the earlier
        // ordinary NUL-bearing key; carrier normalization then makes the latter
        // assignment replace the former.
        apply_environment_update(&indexmap::IndexMap::from([
            ("1\0tail".to_string(), "malformed".to_string()),
            ("1".to_string(), "exact".to_string()),
        ]));
        assert_eq!(
            crate::utils::process_env::var("1").as_deref(),
            Some("malformed")
        );
    }

    /// Maps to: CC `cli/structuredIO.ts:590-602` — the `can_use_tool` control
    /// request's exact payload. Eight keys, no others; the optional four are
    /// dropped by `JSON.stringify` when `undefined`.
    ///
    /// The absence assertions are the point: `title` (removed #123) and
    /// `description` (removed #132) both once rode on this wire and neither
    /// exists in CC's object literal.
    #[test]
    fn can_use_tool_control_request_matches_official_key_set() {
        use crate::types::permissions::{
            PermissionBehavior, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
        };
        use crate::utils::permissions::permission_result::PermissionDecisionReason;

        let mut request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                "perm-sdk",
                "toolu_sdk",
                "Bash",
                "rm -rf /outside",
                serde_json::json!({ "command": "rm -rf /outside" }),
                crate::types::permissions::PermissionMode::Default,
            );
        // The dialog-only strings CC never sends. Both are populated so a
        // re-added key would show up rather than serialize as empty.
        request.description = "Run rm -rf /outside".to_string();
        request.message = "Claude requested permissions to use Bash".to_string();
        request.suggestions = vec![PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![PermissionRuleValue::new("Bash", Some("rm:*".to_string()))],
        }];
        request.blocked_path = Some("/outside".to_string());
        request.decision_reason = Some(PermissionDecisionReason::WorkingDir {
            reason: "path is outside the workspace".to_string(),
        });

        let payload = can_use_tool_control_request(&request, Some("agent-7"));
        let mut keys = payload
            .as_object()
            .expect("control request is an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent_id".to_string(),
                "blocked_path".to_string(),
                "decision_reason".to_string(),
                "input".to_string(),
                "permission_suggestions".to_string(),
                "subtype".to_string(),
                "tool_name".to_string(),
                "tool_use_id".to_string(),
            ],
            "payload={payload}",
        );
        assert_eq!(payload["subtype"], "can_use_tool");
        assert_eq!(payload["tool_name"], "Bash");
        assert_eq!(payload["input"], request.input);
        assert_eq!(payload["tool_use_id"], "toolu_sdk");
        assert_eq!(payload["blocked_path"], "/outside");
        // `serializeDecisionReason` (`structuredIO.ts:64-91`) flattens the
        // union to `reason.reason` for `workingDir`.
        assert_eq!(payload["decision_reason"], "path is outside the workspace");
        assert_eq!(payload["agent_id"], "agent-7");
        assert_eq!(
            payload["permission_suggestions"],
            serde_json::json!([{
                "type": "addRules",
                "destination": "localSettings",
                "behavior": "allow",
                "rules": [{ "toolName": "Bash", "ruleContent": "rm:*" }],
            }]),
        );

        // A main-loop request with nothing optional set: `undefined` values are
        // omitted by `JSON.stringify`, so only the four required keys remain.
        let bare = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-bare",
            "toolu_bare",
            "Bash",
            "ls",
            serde_json::json!({ "command": "ls" }),
            crate::types::permissions::PermissionMode::Default,
        );
        let bare_payload = can_use_tool_control_request(&bare, None);
        let mut bare_keys = bare_payload
            .as_object()
            .expect("control request is an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        bare_keys.sort();
        assert_eq!(
            bare_keys,
            vec![
                "input".to_string(),
                "subtype".to_string(),
                "tool_name".to_string(),
                "tool_use_id".to_string(),
            ],
            "payload={bare_payload}",
        );
    }

    /// Maps to: CC `cli/structuredIO.ts:64-91#serializeDecisionReason`.
    #[test]
    fn serialize_decision_reason_matches_official_switch() {
        use crate::types::permissions::{
            PermissionBehavior, PermissionMode, PermissionRule, PermissionRuleSource,
            PermissionRuleValue,
        };
        use crate::utils::permissions::permission_result::{
            PermissionDecisionReason, SandboxOverrideReason,
        };

        assert_eq!(serialize_decision_reason(None), None);

        // `case 'rule' | 'mode' | 'subcommandResults' | 'permissionPromptTool':
        //  return undefined` (`:77-82`).
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::Rule {
                rule: PermissionRule {
                    source: PermissionRuleSource::Session,
                    rule_behavior: PermissionBehavior::Ask,
                    rule_value: PermissionRuleValue::new("Bash", None),
                },
            })),
            None,
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::Mode {
                mode: PermissionMode::DontAsk,
            })),
            None,
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::SubcommandResults {
                reasons: std::collections::BTreeMap::new(),
            })),
            None,
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::PermissionPromptTool {
                permission_prompt_tool_name: "mcp__approver".to_string(),
                tool_result: serde_json::Value::Null,
            })),
            None,
        );

        // `case 'hook' | 'asyncAgent' | 'sandboxOverride' | 'workingDir' |
        //  'safetyCheck' | 'other': return reason.reason` (`:83-89`).
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: Some("blocked by policy".to_string()),
            })),
            Some("blocked by policy".to_string()),
        );
        // `hook.reason` is optional upstream (`types/permissions.ts:293`).
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::Hook {
                hook_name: "PermissionRequest".to_string(),
                hook_source: None,
                reason: None,
            })),
            None,
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::AsyncAgent {
                reason: "no prompts available".to_string(),
            })),
            Some("no prompts available".to_string()),
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::SandboxOverride {
                reason: SandboxOverrideReason::DangerouslyDisableSandbox,
            })),
            Some("dangerouslyDisableSandbox".to_string()),
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::SafetyCheck {
                reason: "sensitive path".to_string(),
                classifier_approvable: true,
            })),
            Some("sensitive path".to_string()),
        );
        assert_eq!(
            serialize_decision_reason(Some(&PermissionDecisionReason::Other {
                reason: "needs approval".to_string(),
            })),
            Some("needs approval".to_string()),
        );

        // `classifier` is returned only while a classifier feature is on
        // (`:71-76`); with both off CC's switch has no case for it and the
        // function falls off the end.
        let classifier = PermissionDecisionReason::Classifier {
            classifier: "auto-mode".to_string(),
            reason: "3 consecutive actions were blocked".to_string(),
        };
        let classifier_enabled = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::BashClassifier,
        ) || crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
        );
        assert_eq!(
            serialize_decision_reason(Some(&classifier)),
            classifier_enabled.then(|| "3 consecutive actions were blocked".to_string()),
        );
    }

    #[test]
    fn sdk_permission_callback_preserves_updated_input() {
        let request = sdk_test_request();
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"allow",
                    "updatedInput":{"command":"echo updated"},
                })),
                error: None,
            },
            &request,
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::AllowOnce
        );
        assert_eq!(
            response.updated_input,
            Some(serde_json::json!({"command":"echo updated"}))
        );
        assert!(response.permission_updates_explicit);
    }

    /// #170 I4 ordering: the SDK normalizer projects an allow's
    /// `updatedPermissions` into the live AppState ITSELF, before persistence
    /// (`PermissionPromptToolResultSchema.ts:98-105` — `setAppState` at
    /// `:98-104`, `persistPermissionUpdates` at `:105`). Fails on the old
    /// shape: the normalizer only persisted and left the state projection to
    /// the query actor, so the store's `tool_permission_context` stayed
    /// untouched here. A `session` destination keeps `persist` a disk no-op
    /// (`supports_persistence`), isolating the state half.
    #[test]
    fn sdk_permission_allow_projects_updates_into_app_state_in_the_normalizer() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let app_store = crate::tool::AppStoreRef::new(store.clone());
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"allow",
                    "updatedInput":{"command":"echo updated"},
                    "updatedPermissions":[{
                        "type":"addRules",
                        "behavior":"allow",
                        "destination":"session",
                        "rules":[{"toolName":"Bash","ruleContent":"echo updated"}],
                    }],
                })),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &app_store,
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::AllowOnce
        );
        let projected = store.get().tool_permission_context.clone();
        let session_rules = projected
            .always_allow_rules
            .get(&crate::types::permissions::PermissionRuleSource::Session)
            .cloned()
            .unwrap_or_default();
        assert!(
            session_rules.iter().any(|rule| rule.tool_name == "Bash"
                && rule.rule_content.as_deref() == Some("echo updated")),
            "the normalizer must project updatedPermissions into AppState (CC :98-104); got {session_rules:?}"
        );
        // The response still carries the updates for the query actor's
        // in-session projection (the port bridges two containers).
        assert!(response.permission_updates_explicit);
        assert_eq!(response.permission_updates.len(), 1);
    }

    /// #170 I4: the normalize match used to destructure with `..`, dropping
    /// the `permissionPromptTool` decisionReason the normalizer installs
    /// (`PermissionPromptToolResultSchema.ts:90-94`) and the SDK `toolUseID`
    /// (`:60`/`:70`, preserved by CC's `{...result}` spread at `:112-116` /
    /// `:123-126`). Fails on the old shape: the response had no such fields.
    #[test]
    fn sdk_permission_responses_carry_decision_reason_and_tool_use_id() {
        let allow = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"allow",
                    "updatedInput":{"command":"echo updated"},
                    "toolUseID":"toolu_sdk_allow",
                })),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(allow.tool_use_id.as_deref(), Some("toolu_sdk_allow"));
        assert!(matches!(
            allow.decision_reason,
            Some(
                crate::types::permissions::PermissionDecisionReason::PermissionPromptTool {
                    ref permission_prompt_tool_name,
                    ..
                }
            ) if permission_prompt_tool_name == "Bash"
        ));

        let deny = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-2".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"deny",
                    "message":"blocked by host policy",
                    "toolUseID":"toolu_sdk_deny",
                })),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(deny.tool_use_id.as_deref(), Some("toolu_sdk_deny"));
        assert!(matches!(
            deny.decision_reason,
            Some(crate::types::permissions::PermissionDecisionReason::PermissionPromptTool { .. })
        ));
    }

    fn sdk_test_request() -> crate::types::permissions::PermissionRequest {
        crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: "request-1".to_string(),
            tool_use_id: "tool-1".to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Runs a command".to_string(),
            message: String::new(),
            input_summary: "echo old".to_string(),
            input: serde_json::json!({"command":"echo old"}),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new("Bash", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        }
    }

    /// CC returns the SDK deny's raw `message` as the decision that resolves
    /// `canUseTool` (`toolExecution.ts:1023`). Routing it through `feedback`
    /// instead would prepend REJECT_MESSAGE_WITH_REASON_PREFIX to the
    /// model-facing text — the divergence this test pins.
    #[test]
    fn sdk_permission_deny_message_is_a_system_decision_not_feedback() {
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"deny",
                    "message":"blocked by host policy",
                })),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny
        );
        assert_eq!(
            response.decision_message.as_deref(),
            Some("blocked by host policy")
        );
        assert_eq!(response.feedback, None);
    }

    /// CC `PermissionPromptToolResultSchema.ts:117-122` — deny + interrupt
    /// aborts the tool-use controller through the canonical normalizer.
    #[test]
    fn sdk_permission_deny_interrupt_aborts_the_controller() {
        let abort = crate::tool::AbortController::default();
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"deny",
                    "message":"stop",
                    "interrupt":true,
                })),
                error: None,
            },
            &sdk_test_request(),
            &abort,
            &crate::tool::AppStoreRef::default(),
        );
        assert!(abort.is_aborted());
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny
        );
    }

    /// CC's `updatedInput` is REQUIRED on an allow (`z.record`, no
    /// `.optional()`): a missing value fails `schema.parse`, and the catch
    /// converts it into a `Tool permission request failed: …` deny. The old
    /// hand-written parser silently fell back to the original input and ran
    /// the tool.
    #[test]
    fn sdk_permission_allow_without_updated_input_is_a_parse_failure_deny() {
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({"behavior":"allow"})),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny
        );
        let message = response
            .decision_message
            .expect("catch produces a system deny");
        assert!(
            message.starts_with("Tool permission request failed: "),
            "unexpected message: {message}"
        );
    }

    /// Regression (T5 #157): `toolUseID: 7` on an SDK allow used to read as
    /// an ABSENT toolUseID, so the malformed allow proceeded to execution.
    /// CC's `z.string().optional()`
    /// (`PermissionPromptToolResultSchema.ts:60`) rejects the WHOLE response
    /// — `.optional()` admits absence, not a wrong type — and the catch
    /// (`structuredIO.ts:639-649`) turns it into a synthetic deny.
    #[test]
    fn sdk_permission_allow_with_wrong_typed_tool_use_id_is_a_parse_failure_deny() {
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"allow",
                    "updatedInput":{"command":"echo updated"},
                    "toolUseID":7,
                })),
                error: None,
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny,
            "a malformed allow must not run the tool"
        );
        let message = response
            .decision_message
            .expect("catch produces a system deny");
        assert!(
            message.starts_with("Tool permission request failed: "),
            "unexpected message: {message}"
        );
    }

    /// Regression (T5 #157): `interrupt: "yes"` on an SDK deny used to
    /// coerce to false. CC's `z.boolean().optional()`
    /// (`PermissionPromptToolResultSchema.ts:69`) rejects the WHOLE
    /// response; the catch denies with the synthetic message (not the SDK's
    /// own message) and the never-parsed interrupt must NOT abort the
    /// controller.
    #[test]
    fn sdk_permission_deny_with_wrong_typed_interrupt_is_a_parse_failure_deny() {
        let abort = crate::tool::AbortController::default();
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "success".to_string(),
                response: Some(serde_json::json!({
                    "behavior":"deny",
                    "message":"blocked by host policy",
                    "interrupt":"yes",
                })),
                error: None,
            },
            &sdk_test_request(),
            &abort,
            &crate::tool::AppStoreRef::default(),
        );
        assert!(
            !abort.is_aborted(),
            "an interrupt that never parsed must not abort the controller"
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny
        );
        let message = response
            .decision_message
            .expect("catch produces a system deny");
        assert!(
            message.starts_with("Tool permission request failed: "),
            "unexpected message: {message}"
        );
    }

    /// CC `structuredIO.ts:411-413` — an error-subtype control_response
    /// rejects with `new Error(...)`; the catch stringifies it and denies.
    #[test]
    fn sdk_permission_error_subtype_is_a_request_failed_deny() {
        let response = permission_prompt_response_from_control(
            ControlResponseInput {
                request_id: "permission-1".to_string(),
                subtype: "error".to_string(),
                response: None,
                error: Some("host went away".to_string()),
            },
            &sdk_test_request(),
            &crate::tool::AbortController::default(),
            &crate::tool::AppStoreRef::default(),
        );
        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::Deny
        );
        assert_eq!(
            response.decision_message.as_deref(),
            Some("Tool permission request failed: Error: host went away")
        );
    }

    /// A bridge whose outbound leg (CC `StructuredIO.outbound`) is captured
    /// so tests can observe the `control_request`/`control_cancel_request`
    /// messages instead of them going to stdout.
    fn bridge_with_captured_outbound() -> (
        SdkControlBridge,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();
        let bridge = SdkControlBridge {
            pending: PendingControlResponses::default(),
            input_closed: std::sync::Arc::default(),
            emit: std::sync::Arc::new(move |value| {
                sink.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(value);
            }),
        };
        (bridge, captured)
    }

    /// Bound a race that is supposed to be resolved by a hook. With no SDK
    /// response in flight the loop in `request_sdk_tool_permission` polls
    /// forever, so a hook that never runs would hang the suite instead of
    /// failing it.
    async fn race_timeout<F>(future: F) -> F::Output
    where
        F: std::future::Future,
    {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect("a hook decision must resolve the race")
    }

    /// Bound a control request that stdin EOF is supposed to resolve. The bound
    /// is the whole reason the close-tail tests below are regressions rather
    /// than a wedged suite: on the shape they pin (no `close_input`, no
    /// `inputClosed` guard) the parked `recv()` NEVER wakes, so an unbounded
    /// await would hang `just test` instead of failing it. The assertions that
    /// follow each call are about the decision CC produces, not about the
    /// await having returned.
    async fn eof_timeout<F>(future: F) -> F::Output
    where
        F: std::future::Future,
    {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect(
                "stdin EOF must resolve the parked control request \
                 (CC cli/structuredIO.ts:254-260 / :480-482)",
            )
    }

    /// Register a single Callback-type PermissionRequest hook returning the
    /// given HookJSONOutput value (CC SDK hosts register these through the
    /// initialize request; the race consumes them like any registered hook).
    fn register_permission_request_hook(output: serde_json::Value) {
        let mut hooks = std::collections::HashMap::new();
        hooks.insert(
            "PermissionRequest".to_string(),
            vec![crate::schemas::hooks::RegisteredHookMatcher {
                matcher: None,
                hooks: vec![crate::schemas::hooks::RegisteredHook::Callback(
                    crate::schemas::hooks::HookCallback {
                        callback: std::sync::Arc::new(move |_input, _tool_use_id| {
                            let output = output.clone();
                            Box::pin(async move { output })
                        }),
                        timeout: Some(5),
                    },
                )],
                plugin_root: None,
                plugin_name: None,
                plugin_id: None,
            }],
        );
        crate::bootstrap::state::clear_registered_hooks();
        crate::bootstrap::state::register_hook_callbacks(hooks);
    }

    /// CC `structuredIO.ts:490-504` — an abort during the SDK prompt first
    /// enqueues an outbound `control_cancel_request` (releasing the host's
    /// hanging canUseTool callback), then rejects with `AbortError`, and the
    /// catch denies. The port used to only remove the local pending entry —
    /// no cancel message ever reached the host.
    #[test]
    fn sdk_permission_abort_during_prompt_is_a_request_failed_deny() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            crate::bootstrap::state::clear_registered_hooks();
            let (bridge, outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();
            abort.abort();
            let response = request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &abort,
                None,
                &crate::tool::AppStoreRef::default(),
            )
            .await;
            assert_eq!(
                response.choice,
                crate::types::permissions::PermissionPromptChoice::Deny
            );
            assert_eq!(
                response.decision_message.as_deref(),
                Some("Tool permission request failed: AbortError")
            );
            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let request_id = outbound[0]["request_id"]
                .as_str()
                .expect("first outbound message is the control_request")
                .to_string();
            assert!(
                outbound.iter().any(|message| {
                    message["type"] == "control_cancel_request"
                        && message["request_id"] == serde_json::json!(request_id)
                }),
                "abort must enqueue an outbound control_cancel_request: {outbound:?}"
            );
            assert!(
                bridge
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty(),
                "the local pending entry is dropped immediately"
            );
        });
    }

    /// CC `structuredIO.ts:561-619` — the PermissionRequest hooks race the
    /// SDK prompt; a hook decision wins, the pending SDK request is aborted
    /// (outbound `control_cancel_request` + local entry dropped), and the
    /// hook decision resolves the callback. The port used to await the SDK
    /// response alone — a hook decision could never win.
    #[test]
    fn sdk_permission_hook_decision_wins_race_and_cancels_sdk_request() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            register_permission_request_hook(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedInput": {"command": "echo hooked"}
                    }
                }
            }));
            let (bridge, outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();
            // No SDK response is ever delivered: only the hook can resolve.
            let response = request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &abort,
                None,
                &crate::tool::AppStoreRef::default(),
            )
            .await;
            crate::bootstrap::state::clear_registered_hooks();
            assert_eq!(
                response.choice,
                crate::types::permissions::PermissionPromptChoice::AllowOnce
            );
            assert_eq!(
                response.updated_input,
                Some(serde_json::json!({"command": "echo hooked"})),
                "the hook decision carries finalInput = updatedInput || input"
            );
            assert!(matches!(
                response.decision_reason,
                Some(crate::types::permissions::PermissionDecisionReason::Hook { ref hook_name, .. })
                    if hook_name == "PermissionRequest"
            ));
            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(
                outbound[0]["type"], "control_request",
                "the SDK prompt is sent immediately, before the hooks resolve"
            );
            let request_id = outbound[0]["request_id"].as_str().unwrap().to_string();
            assert!(
                outbound.iter().any(|message| {
                    message["type"] == "control_cancel_request"
                        && message["request_id"] == serde_json::json!(request_id)
                }),
                "a winning hook decision must cancel the pending SDK request: {outbound:?}"
            );
            assert!(
                bridge
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty()
            );
        });
    }

    /// CC `structuredIO.ts:620-628` — a hook that passes through (no
    /// decision) leaves the SDK prompt as the resolver; its response is
    /// honored and no cancel is sent.
    #[test]
    fn sdk_permission_hook_passthrough_honors_sdk_response() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            // A hook with no decision: executePermissionRequestHooksForSDK
            // resolves undefined and the race keeps awaiting the SDK.
            register_permission_request_hook(serde_json::json!({}));
            let (bridge, outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();
            let pending = bridge.pending.clone();
            let responder = tokio::spawn(async move {
                loop {
                    let entry = {
                        let mut pending = pending
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let key = pending.keys().next().cloned();
                        key.and_then(|key| pending.remove(&key).map(|sender| (key, sender)))
                    };
                    if let Some((request_id, sender)) = entry {
                        let _ = sender
                            .send(ControlResponseInput {
                                request_id,
                                subtype: "success".to_string(),
                                response: Some(serde_json::json!({
                                    "behavior": "allow",
                                    "updatedInput": {"command": "echo from-sdk"},
                                })),
                                error: None,
                            })
                            .await;
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            });
            let response = request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &abort,
                None,
                &crate::tool::AppStoreRef::default(),
            )
            .await;
            responder.await.unwrap();
            crate::bootstrap::state::clear_registered_hooks();
            assert_eq!(
                response.choice,
                crate::types::permissions::PermissionPromptChoice::AllowOnce
            );
            assert_eq!(
                response.updated_input,
                Some(serde_json::json!({"command": "echo from-sdk"})),
                "hook pass-through must honor the SDK response"
            );
            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(
                !outbound
                    .iter()
                    .any(|message| message["type"] == "control_cancel_request"),
                "no cancel when the SDK response resolves the race: {outbound:?}"
            );
        });
    }

    /// CC assembles the race's hook set through
    /// `executeHooks` → `getMatchingHooks(appState, toolUseContext?.agentId ??
    /// getSessionId(), …)` → `getHooksConfig` (`hooks.ts:2003-2010`,
    /// `:1491-1565`), which merges `appState.sessionHooks` for that session on
    /// top of the settings snapshot and the registered hooks. An agent's
    /// frontmatter registrations (CC `runAgent.ts:568`
    /// `registerFrontmatterHooks`) therefore race like any other hook.
    ///
    /// Old shape (verified by reverting the merge, 2026-08-29):
    /// `execute_permission_request_hooks_for_sdk` read `load_hooks_config()`
    /// alone — the two process-level sources — so with no settings/registered
    /// PermissionRequest hook the `has_hooks` guard returned `None` at once and
    /// this deny never ran. The race then had no resolver left (the test sends
    /// no SDK response), so it spun in its 25 ms poll loop forever; the
    /// `race_timeout` below is what turns that into a failure instead of a
    /// hung suite.
    #[test]
    fn sdk_permission_race_runs_session_registered_hooks() {
        use crate::services::hooks::{HookCommand, HookEvent};
        use crate::utils::hooks::session_hooks;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            crate::bootstrap::state::clear_registered_hooks();
            session_hooks::clear_all_session_hooks();
            // Registered under the AGENT id: CC's session id for a subagent's
            // hooks is `toolUseContext.agentId` (`hooks.ts:2003`).
            session_hooks::add_session_hook(
                "agent-7",
                HookEvent::PermissionRequest,
                "Bash",
                HookCommand {
                    command: r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"denied by session hook"}}}'"#
                        .to_string(),
                    shell: None,
                    timeout: Some(5),
                    condition: None,
                    status: None,
                    once: None,
                    is_async: None,
                    async_rewake: None,
                },
            );
            let (bridge, outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();
            // No SDK response is ever delivered: only the hook can resolve.
            let response = race_timeout(request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &abort,
                Some("agent-7"),
                &crate::tool::AppStoreRef::default(),
            ))
            .await;
            session_hooks::clear_all_session_hooks();
            assert_eq!(
                response.choice,
                crate::types::permissions::PermissionPromptChoice::Deny
            );
            assert_eq!(
                response.decision_message.as_deref(),
                Some("denied by session hook"),
                "a session-registered hook must join the race, not be skipped"
            );
            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let request_id = outbound[0]["request_id"].as_str().unwrap().to_string();
            assert!(
                outbound.iter().any(|message| {
                    message["type"] == "control_cancel_request"
                        && message["request_id"] == serde_json::json!(request_id)
                }),
                "the winning session hook cancels the pending SDK request: {outbound:?}"
            );
        });
    }

    /// The same merge, for the MAIN session: CC's `?? getSessionId()` fallback
    /// (`hooks.ts:2003`) means a hook the main thread registered also races.
    /// Old shape: skipped for the same reason as the agent case.
    #[test]
    fn sdk_permission_race_runs_main_session_registered_hooks() {
        use crate::services::hooks::{HookCommand, HookEvent};
        use crate::utils::hooks::session_hooks;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            crate::bootstrap::state::clear_registered_hooks();
            session_hooks::clear_all_session_hooks();
            session_hooks::add_session_hook(
                &crate::bootstrap::state::get_session_id(),
                HookEvent::PermissionRequest,
                "Bash",
                HookCommand {
                    command: r#"printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"denied by main-session hook"}}}'"#
                        .to_string(),
                    shell: None,
                    timeout: Some(5),
                    condition: None,
                    status: None,
                    once: None,
                    is_async: None,
                    async_rewake: None,
                },
            );
            let (bridge, _outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();
            let response = race_timeout(request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &abort,
                None,
                &crate::tool::AppStoreRef::default(),
            ))
            .await;
            session_hooks::clear_all_session_hooks();
            assert_eq!(
                response.choice,
                crate::types::permissions::PermissionPromptChoice::Deny
            );
            assert_eq!(
                response.decision_message.as_deref(),
                Some("denied by main-session hook"),
                "the `?? getSessionId()` arm must merge too"
            );
        });
    }

    /// Maps to: CC `structuredIO.handleElicitation` — success responses map
    /// the validated action/content; every failure shape resolves to cancel.
    #[test]
    fn sdk_elicitation_forwards_and_maps_responses_like_official() {
        use crate::services::mcp::elicitation_handler::{
            ElicitationAction, ElicitationRequestParams,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = SdkControlBridge::default();

            let respond_with = |bridge: &SdkControlBridge, response: ControlResponseInput| {
                let pending = bridge.pending.clone();
                tokio::spawn(async move {
                    loop {
                        let sender = {
                            let mut pending = pending
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            let key = pending.keys().next().cloned();
                            key.and_then(|key| pending.remove(&key))
                        };
                        if let Some(sender) = sender {
                            let _ = sender.send(response).await;
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };

            // A success response carries action + content through.
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "elicit-1".to_string(),
                    subtype: "success".to_string(),
                    response: Some(serde_json::json!({
                        "action": "accept",
                        "content": {"token": "abc"},
                    })),
                    error: None,
                },
            );
            let result = request_sdk_elicitation(
                &bridge,
                "docs-server",
                ElicitationRequestParams::Url {
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: Some("elicit-1".to_string()),
                },
            )
            .await;
            responder.await.unwrap();
            assert_eq!(result.action, ElicitationAction::Accept);
            assert_eq!(result.content, Some(serde_json::json!({"token": "abc"})));

            // An error subtype resolves to cancel (CC's catch).
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "elicit-2".to_string(),
                    subtype: "error".to_string(),
                    response: None,
                    error: Some("stream closed".to_string()),
                },
            );
            let result = request_sdk_elicitation(
                &bridge,
                "docs-server",
                ElicitationRequestParams::Form {
                    message: "Fill in".to_string(),
                    requested_schema: serde_json::json!({}),
                },
            )
            .await;
            responder.await.unwrap();
            assert_eq!(result.action, ElicitationAction::Cancel);

            // A malformed action fails schema validation → cancel.
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "elicit-3".to_string(),
                    subtype: "success".to_string(),
                    response: Some(serde_json::json!({"action": "maybe"})),
                    error: None,
                },
            );
            let result = request_sdk_elicitation(
                &bridge,
                "docs-server",
                ElicitationRequestParams::Url {
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: None,
                },
            )
            .await;
            responder.await.unwrap();
            assert_eq!(result.action, ElicitationAction::Cancel);
        });
    }

    /// Maps to: CC `structuredIO.createHookCallback` — a success response
    /// passes hookJSONOutputSchema and comes back verbatim; error subtype,
    /// closed channel, and schema-invalid responses all resolve to `{}`.
    #[test]
    fn sdk_hook_callback_validates_responses_like_official() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = SdkControlBridge::default();
            let respond_with = |bridge: &SdkControlBridge, response: ControlResponseInput| {
                let pending = bridge.pending.clone();
                tokio::spawn(async move {
                    loop {
                        let sender = {
                            let mut pending = pending
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            let key = pending.keys().next().cloned();
                            key.and_then(|key| pending.remove(&key))
                        };
                        if let Some(sender) = sender {
                            let _ = sender.send(response).await;
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };

            // The callback wrapper threads input/tool_use_id through.
            let callback = create_hook_callback(&bridge, "cb-1".to_string(), Some(30));
            assert_eq!(callback.timeout, Some(30));
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "hook-1".to_string(),
                    subtype: "success".to_string(),
                    response: Some(serde_json::json!({
                        "decision": "block",
                        "reason": "not now",
                    })),
                    error: None,
                },
            );
            let value = (callback.callback)(
                serde_json::json!({"hook_event_name": "PreToolUse"}),
                Some("tool-1".to_string()),
            )
            .await;
            responder.await.unwrap();
            assert_eq!(value["decision"], "block");
            assert_eq!(value["reason"], "not now");

            // A schema-invalid response resolves to {} (CC's catch).
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "hook-2".to_string(),
                    subtype: "success".to_string(),
                    response: Some(serde_json::json!({"decision": "maybe"})),
                    error: None,
                },
            );
            let value =
                request_sdk_hook_callback(&bridge, "cb-1", serde_json::json!({}), None).await;
            responder.await.unwrap();
            assert_eq!(value, serde_json::json!({}));

            // An error subtype resolves to {}.
            let responder = respond_with(
                &bridge,
                ControlResponseInput {
                    request_id: "hook-3".to_string(),
                    subtype: "error".to_string(),
                    response: None,
                    error: Some("gone".to_string()),
                },
            );
            let value =
                request_sdk_hook_callback(&bridge, "cb-1", serde_json::json!({}), None).await;
            responder.await.unwrap();
            assert_eq!(value, serde_json::json!({}));
        });
    }

    /// CC has exactly one producer for outbound control requests: every
    /// `subtype` — `can_use_tool` (`:590-602`), `hook_callback` (`:671-680`)
    /// and `elicitation` (`:704-716`) — is built and handed to `sendRequest`,
    /// whose only write is `this.outbound.enqueue(message)` (`:486`).
    ///
    /// Old shape: `request_sdk_hook_callback` and `request_sdk_elicitation`
    /// called `json_line` directly. Production was identical (the default
    /// `emit` IS `json_line`), so nothing was observably wrong — but the two
    /// legs escaped the capture seam, which is why only the permission leg had
    /// wire-level coverage. On the old shape this test FAILS rather than hangs:
    /// the responder resolves the request either way and `outbound` is empty.
    #[test]
    fn every_sdk_control_request_leaves_through_the_outbound_sink() {
        use crate::services::mcp::elicitation_handler::ElicitationRequestParams;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (bridge, outbound) = bridge_with_captured_outbound();
            let respond_with = |bridge: &SdkControlBridge, response: serde_json::Value| {
                let pending = bridge.pending.clone();
                tokio::spawn(async move {
                    loop {
                        let entry = {
                            let mut pending = pending
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            let key = pending.keys().next().cloned();
                            key.and_then(|key| pending.remove(&key).map(|sender| (key, sender)))
                        };
                        if let Some((request_id, sender)) = entry {
                            let _ = sender
                                .send(ControlResponseInput {
                                    request_id,
                                    subtype: "success".to_string(),
                                    response: Some(response),
                                    error: None,
                                })
                                .await;
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };

            let responder = respond_with(&bridge, serde_json::json!({}));
            request_sdk_hook_callback(
                &bridge,
                "cb-1",
                serde_json::json!({"hook_event_name": "PreToolUse"}),
                Some("toolu_1".to_string()),
            )
            .await;
            responder.await.unwrap();

            let responder = respond_with(&bridge, serde_json::json!({"action": "cancel"}));
            request_sdk_elicitation(
                &bridge,
                "docs-server",
                ElicitationRequestParams::Url {
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: Some("elicit-1".to_string()),
                },
            )
            .await;
            responder.await.unwrap();

            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let subtypes = outbound
                .iter()
                .map(|message| {
                    (
                        message["type"].as_str().unwrap_or_default().to_string(),
                        message["request"]["subtype"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                subtypes,
                vec![
                    ("control_request".to_string(), "hook_callback".to_string()),
                    ("control_request".to_string(), "elicitation".to_string()),
                ],
                "both legs must ride the same sink as can_use_tool: {outbound:?}"
            );
            assert_eq!(outbound[0]["request"]["callback_id"], "cb-1");
            assert_eq!(outbound[0]["request"]["tool_use_id"], "toolu_1");
            assert_eq!(outbound[1]["request"]["mcp_server_name"], "docs-server");
            assert_eq!(outbound[1]["request"]["mode"], "url");
            // `request_id` is the correlation key `sendRequest` keys
            // `pendingRequests` on (`:512`); each request carries its own.
            for message in outbound.iter() {
                let request_id = message["request_id"].as_str().unwrap_or_default();
                assert!(!request_id.is_empty(), "no request_id on {message}");
            }
            assert_ne!(outbound[0]["request_id"], outbound[1]["request_id"]);
        });
    }

    /// Maps to: CC `cli/structuredIO.ts:254-260` — `read()`'s close tail
    /// rejects EVERY entry of `pendingRequests`, so all three registrants are
    /// freed at stdin EOF, each with the value its own caller's catch produces:
    /// the stream-closed deny for `can_use_tool` (`:639-649`), `{}` for
    /// `hook_callback` (`:682-686`) and `{action:'cancel'}` for `elicitation`
    /// (`:718-720`).
    ///
    /// Old shape: nothing called the close tail — the `sdk-stdin-reader` thread
    /// just returned at EOF — so each parked `Sender` kept its channel open and
    /// all three `recv()`s waited FOREVER. An SDK host that closed stdin with a
    /// permission request in flight hung the port where CC terminates it.
    /// That failure mode is a hang, which is why the await is bounded: with
    /// `close_input`'s body emptied (verified 2026-08-30) this test fails on
    /// `eof_timeout`'s expect and the suite still finishes.
    #[test]
    fn stdin_eof_frees_every_parked_control_request() {
        use crate::services::mcp::elicitation_handler::{
            ElicitationAction, ElicitationRequestParams,
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            crate::bootstrap::state::clear_registered_hooks();
            crate::utils::hooks::session_hooks::clear_all_session_hooks();
            let (bridge, outbound) = bridge_with_captured_outbound();
            let abort = crate::tool::AbortController::default();

            // Stands in for the reader thread: close once all three requests
            // have parked, which is what `run_stream_json`'s `sdk-stdin-reader`
            // does when `next_stream_json_line` returns `None`.
            let closer = {
                let bridge = bridge.clone();
                tokio::spawn(async move {
                    loop {
                        let parked = bridge
                            .pending
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .len();
                        if parked == 3 {
                            bridge.close_input();
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };

            let request = sdk_test_request();
            let app_store = crate::tool::AppStoreRef::default();
            let (permission, hook, elicitation) = eof_timeout(async {
                tokio::join!(
                    request_sdk_tool_permission(&bridge, &request, &abort, None, &app_store),
                    request_sdk_hook_callback(
                        &bridge,
                        "cb-1",
                        serde_json::json!({"hook_event_name": "PreToolUse"}),
                        None,
                    ),
                    request_sdk_elicitation(
                        &bridge,
                        "docs-server",
                        ElicitationRequestParams::Form {
                            message: "Fill in".to_string(),
                            requested_schema: serde_json::json!({}),
                        },
                    ),
                )
            })
            .await;
            closer.await.unwrap();

            assert_eq!(
                permission.choice,
                crate::types::permissions::PermissionPromptChoice::Deny
            );
            assert_eq!(
                permission.decision_message.as_deref(),
                Some(
                    "Tool permission request failed: Error: Tool permission stream closed \
                     before response received"
                ),
                "the close tail rejects with that Error and `createCanUseTool`'s catch \
                 (`:639-649`) stringifies it"
            );
            assert_eq!(hook, serde_json::json!({}));
            assert_eq!(elicitation.action, ElicitationAction::Cancel);

            let outbound = outbound
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(
                outbound.len(),
                3,
                "all three were sent before the close; the close tail adds no traffic \
                 of its own — CC rejects the promises directly, it does not enqueue \
                 `control_cancel_request`s: {outbound:?}"
            );
            assert!(
                !outbound
                    .iter()
                    .any(|message| message["type"] == "control_cancel_request"),
                "{outbound:?}"
            );
            assert!(
                bridge
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty()
            );
        });
    }

    /// Maps to: CC `cli/structuredIO.ts:480-482` — past EOF `sendRequest`
    /// throws `new Error('Stream closed')` BEFORE `outbound.enqueue` (`:486`),
    /// so a request raised after the close sends nothing and resolves at once
    /// through the same three catches.
    ///
    /// This is the half of the hang the close tail alone would not reach:
    /// `run_stream_json` awaits a whole `ask()` turn, so a query keeps running
    /// — and keeps asking for permissions — after the reader thread has hit EOF
    /// and gone away. Old shape: each of these three parks on a map with no
    /// remaining producer and never returns; `eof_timeout` is what makes that a
    /// failure instead of a wedged suite.
    #[test]
    fn control_requests_raised_after_stdin_eof_resolve_without_parking() {
        use crate::services::mcp::elicitation_handler::{
            ElicitationAction, ElicitationRequestParams,
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            crate::bootstrap::state::clear_registered_hooks();
            crate::utils::hooks::session_hooks::clear_all_session_hooks();
            let (bridge, outbound) = bridge_with_captured_outbound();
            bridge.close_input();

            let permission = eof_timeout(request_sdk_tool_permission(
                &bridge,
                &sdk_test_request(),
                &crate::tool::AbortController::default(),
                None,
                &crate::tool::AppStoreRef::default(),
            ))
            .await;
            assert_eq!(
                permission.choice,
                crate::types::permissions::PermissionPromptChoice::Deny
            );
            assert_eq!(
                permission.decision_message.as_deref(),
                Some("Tool permission request failed: Error: Stream closed"),
                "CC's guard throws `Stream closed`, not the close tail's message"
            );

            let hook = eof_timeout(request_sdk_hook_callback(
                &bridge,
                "cb-1",
                serde_json::json!({}),
                None,
            ))
            .await;
            assert_eq!(hook, serde_json::json!({}));

            let elicitation = eof_timeout(request_sdk_elicitation(
                &bridge,
                "docs-server",
                ElicitationRequestParams::Url {
                    message: "Authorize".to_string(),
                    url: "https://example.com/auth".to_string(),
                    elicitation_id: None,
                },
            ))
            .await;
            assert_eq!(elicitation.action, ElicitationAction::Cancel);

            assert!(
                outbound
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty(),
                "CC's `:480-482` guard sits ahead of `outbound.enqueue` (`:486`), so a \
                 closed stream is never written to"
            );
            assert!(
                bridge
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty(),
                "and ahead of `pendingRequests.set` (`:512`), so nothing parks"
            );
        });
    }
}
