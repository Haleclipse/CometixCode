//! UI-only subset of official
//! `hooks/toolPermission/handlers/interactiveHandler.ts`.
//! Officially this handler pushes a `ToolUseConfirm` item and leaves the outer
//! permission promise unresolved until the user allows/rejects. Here it only
//! pushes a `ToolUseConfirm` into REPL's queue; the pending query pump remains
//! blocked while the queue is non-empty.

use crate::hooks::tool_permission::permission_context::push_to_queue;
use crate::types::permissions::{
    PermissionPromptChoice, PermissionPromptResponse, PermissionRequest, ToolUseConfirm,
};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub fn handle_interactive_permission(queue: &mut Vec<ToolUseConfirm>, request: PermissionRequest) {
    push_to_queue(
        queue,
        tool_use_confirm_for_request(
            request,
            crate::types::permissions::PermissionPromptResponder::default(),
            None,
        ),
    );
}

/// Builds the queue entry `handle_interactive_permission` pushes.
///
/// Maps to: CC `interactiveHandler.ts:57` `handleInteractivePermission(...,
/// resolve)` constructing the `ToolUseConfirm`
/// (`components/permissions/PermissionRequest.tsx:137-166`) — `description` and
/// `permissionPromptStartTimeMs = Date.now()` are stamped here, `resolve`
/// becomes the entry's `onAllow`/`onReject` (`:158-164`), and
/// `asking_tool_permission_context` is CC's `toolUseContext: ctx.toolUseContext`
/// (`:97`), supplied by the asker because only the asker knows it.
///
/// Split out from the push so a producer that is not the component owning the
/// queue can build the same entry and hand it over; the entry is identical
/// either way, which is the point — CC has one `ToolUseConfirm` type and one
/// queue (`REPL.tsx:1529`) for every asking caller.
pub fn tool_use_confirm_for_request(
    mut request: PermissionRequest,
    responder: crate::types::permissions::PermissionPromptResponder,
    asking_tool_permission_context: Option<crate::tool::ToolPermissionContext>,
) -> ToolUseConfirm {
    let is_in_process_teammate = crate::utils::teammate_context::get_teammate_context()
        .is_some_and(|identity| identity.is_in_process);
    // CC useCanUseTool.tsx:138-143 computes description from ctx.input first.
    fill_tool_description(&mut request);
    // CC interactiveHandler.ts:84 vs inProcessRunner.ts:215-235: only the
    // interactive builder uses displayInput. Teammates retain original input.
    if !is_in_process_teammate
        && let Some(crate::types::permissions::PermissionDecision::Ask {
            updated_input: Some(input),
            ..
        }) = &request.permission_result
    {
        request.input = input.clone();
    }
    let mut confirm = ToolUseConfirm::new(request)
        .with_permission_prompt_start_time_ms(now_ms())
        .with_responder(responder)
        .with_asking_tool_permission_context(asking_tool_permission_context);
    // Maps to: CC's choice of BUILDER — `createInProcessCanUseTool`
    // (`inProcessRunner.ts:223-332`) rather than `handleInteractivePermission`
    // (`interactiveHandler.ts:92-232`). The condition under which CC is inside
    // the former is "an in-process teammate is asking", which this port reads
    // off the teammate task scope (CC's `teammateContext.ts` ALS) — the same
    // test `run_agent.rs#ask_parent_for_agent_permission` uses one frame up to
    // pick the worker badge. Both behaviours that differ between the two
    // builders hang off this one fact; see [`PermissionRowSource`].
    confirm.source = if is_in_process_teammate {
        crate::types::permissions::PermissionRowSource::InProcessTeammate
    } else {
        crate::types::permissions::PermissionRowSource::Interactive
    };
    confirm
}

/// Builds the `ask` leg of CC's `canUseTool` for a REPL that owns the
/// `ToolUseConfirm` queue.
///
/// Maps to: CC `hooks/useCanUseTool.tsx:55-79,189-327` — `useCanUseTool` closes
/// over `setToolUseConfirmQueue`, calls `handleInteractivePermission(..., resolve)`
/// on `ask`, and the queued entry's `onAllow`/`onReject` resolve the promise the
/// caller is awaiting. The REPL hands the resulting closure to `query(...)` as
/// `canUseTool` (`REPL.tsx:3137`, `:3409`), which is how a subagent gets it
/// (`AgentTool.tsx:399` → `:879` → `runAgent.ts:753`).
///
/// The entry reaches the REPL through the bridge setter
/// (`utils/swarm/leaderPermissionBridge.ts#SetToolUseConfirmQueueFn`) because
/// Nyan evaluates permissions on query-actor threads while CC's closure runs
/// in-component and can call `setToolUseConfirmQueue` directly. It is the
/// ordinary `ToolUseConfirm`, in the ordinary queue, appended by the ordinary
/// updater — the same setter `inProcessRunner.ts:223` pushes through.
///
/// `None` from the returned future means the REPL is gone (the applier dropped
/// the entry, or it was dropped without an answer) — callers fall back to their
/// no-prompt behaviour rather than parking, the same rule
/// `repl_sandbox_ask_flow` applies.
pub fn create_repl_interactive_permission_sink(
    set_tool_use_confirm_queue: impl Into<
        crate::utils::swarm::leader_permission_bridge::SetToolUseConfirmQueueFn,
    >,
) -> crate::tool::InteractivePermissionSink {
    let set_tool_use_confirm_queue = set_tool_use_confirm_queue.into();
    crate::tool::InteractivePermissionSink::new(
        move |ask: crate::tool::InteractivePermissionAsk| {
            let set_tool_use_confirm_queue = set_tool_use_confirm_queue.clone();
            Box::pin(async move {
                let (response_tx, response_rx) = async_channel::bounded(1);
                let mut confirm = tool_use_confirm_for_request(
                    ask.request,
                    crate::types::permissions::PermissionPromptResponder::new(response_tx),
                    // Maps to: CC `interactiveHandler.ts:97` /
                    // `inProcessRunner.ts:230` — the row records the context of
                    // the caller that is parked on the promise, so
                    // `recheckPermission` re-evaluates against THAT context.
                    ask.asking_tool_permission_context,
                );
                // Maps to: CC `inProcessRunner.ts:229-232` — the in-process
                // teammate's queue entry carries `workerBadge` so the leader
                // dialog shows who is asking.
                if let Some(worker_badge) = ask.worker_badge {
                    confirm = confirm.with_worker_badge(worker_badge);
                }
                set_tool_use_confirm_queue.push_to_queue(confirm);
                response_rx.recv().await.ok()
            })
        },
    )
}

/// Maps to: CC `interactiveHandler.ts:204-231` `async recheckPermission()`, the
/// queued entry's own re-evaluation callback, and `inProcessRunner.ts:305-330`,
/// the one an in-process teammate installs on ITS entry:
///
/// ```ts
/// async recheckPermission() {
///   if (isResolved()) return                      // :205 / :306 `if (decisionMade) return`
///   const freshResult = await hasPermissionsToUseTool(tool, input, toolUseContext, assistantMessage, toolUseID)
///   if (freshResult.behavior === 'allow') {
///     if (!claim()) return                        // :222 / :315 `decisionMade = true`
///     ctx.removeFromQueue()                       // :227 / :321-323 filter by toolUseID
///     resolveOnce(ctx.buildAllow(freshResult.updatedInput ?? ctx.input))   // :229
///   }
/// }
/// ```
///
/// One function for CC's two copies because this port has ONE
/// `ToolUseConfirm` type and one queue for every asking caller
/// (`types/permissions.rs`) — but the two copies are **not byte-equivalent**,
/// and #179 merging them on that claim was wrong. What they resolve WITH
/// differs, and the difference is not incidental:
///
/// ```ts
/// // interactiveHandler.ts:229
/// resolveOnce(ctx.buildAllow(freshResult.updatedInput ?? ctx.input))
/// // inProcessRunner.ts:324-328
/// resolve({ ...freshResult, updatedInput: input, userModified: false })
/// ```
///
/// (both quoted from `ast-grep run --lang ts --pattern
/// 'resolveOnce(ctx.buildAllow($$$))'` and `--pattern 'resolve({ ...freshResult,
/// updatedInput: input, userModified: false })'`.) In the teammate copy the
/// spread comes FIRST and the explicit `updatedInput: input` overrides it, so
/// `freshResult.updatedInput` is discarded UNCONDITIONALLY — the tool re-runs
/// on the model's original input. The interactive copy takes the fresh value
/// when there is one, and only falls back to the original through `??`. The
/// same asymmetry decides `decisionReason`: `ctx.buildAllow(input)` with no
/// `opts` builds `{ behavior, updatedInput, userModified }` and nothing else
/// (`PermissionContext.ts:264-284`; contrast `:493`, which passes
/// `{ decisionReason }` explicitly), while the teammate copy's spread carries
/// `freshResult.decisionReason` through. Both are parameterised below off
/// [`crate::types::permissions::PermissionRowSource`], which is the same
/// discriminator CC uses: which builder made the row.
///
/// `acceptFeedback`/`contentBlocks` ride the spread in principle and are not
/// ported into it, because `hasPermissionsToUseTool` never produces them —
/// `grep -n "acceptFeedback\|contentBlocks" src/utils/permissions/permissions.ts`
/// is empty; they exist only on decisions built by `handleUserAllow` /
/// `cancelAndAbort`.
///
/// Three statements of the interactive copy have no counterpart here, each
/// ruled rather than ported:
///
/// - `:223-225` `bridgeCallbacks.cancelRequest(bridgeRequestId)` — the CCR
///   bridge (`bridge/bridgePermissionCallbacks.ts`) is unported; there is no
///   remote prompt to dismiss. It comes back with the bridge, not before.
/// - `:226` `channelUnsubscribe?.()` — the channel relay IS ported, but its
///   registration is owned by the asking query
///   (`query.rs:3208` holds a [`ChannelPermissionRelayRegistration`] whose
///   `Drop` unsubscribes), so the row resolving here drops the asker's
///   registration by RAII on the same event. There is no per-row handle to
///   call, and adding one would duplicate the owner.
/// - `:228` `ctx.logDecision({ decision: 'accept', source: 'config' })` —
///   analytics. Standing ruling: `logEvent` bodies are bookkeeping only
///   (`permission_logging.rs` records the event names without sending), and
///   `PermissionDecisionLogArgs::AcceptConfig` is already the entry for this
///   exact call. No decision rides on it.
///
/// `ctx.toolUseContext` is the ASKER's context, and the entry carries it
/// (`ToolUseConfirm.asking_tool_permission_context`, CC's `toolUseContext`
/// field at `PermissionRequest.tsx:142`, installed at the push by
/// `interactiveHandler.ts:97` / `inProcessRunner.ts:230`). `None` means the
/// asker is the REPL itself, whose context is the leader's live one.
///
/// That distinction is load-bearing, not decorative. #179 claimed the
/// difference was conservative — "the transform only ever widens the mode, so
/// this can miss a re-allow, never invent one". **That claim does not hold**,
/// in two independent ways, both read off `agentGetAppState` itself:
///
/// 1. `runAgent.ts:421-434` overrides the mode with
///    `agentDefinition.permissionMode` whenever the parent is NOT in
///    `bypassPermissions` / `acceptEdits` / `auto`. A `plan` agent under a
///    `default` leader is therefore NARROWER than the leader, not wider
///    (`run_agent.rs#agent_permission_context_for_run`, same branch).
/// 2. `runAgent.ts:469-478` REPLACES `alwaysAllowRules` with
///    `{ cliArg: <parent's cliArg>, session: allowedTools }` when
///    `allowedTools` is provided. The parent's `user`/`project`/`local`/
///    `flag`/`policy`/`command`/`session` allow rules are dropped from the
///    agent's context — so the leader's live context is strictly wider there,
///    and a recheck against it could allow a tool the agent's own context would
///    still have asked about. This is not a mode widening at all.
///
/// Deliberate narrowings, both stated rather than hidden:
///
/// - **Only entries that carry a resolver can be resolved.** Every production
///   row now carries one — a nested query's over the sink's channel, the REPL's
///   own over `QueryCommand::PermissionResponse` to the handle that raised it
///   (`screens/repl.rs`, `QueryEvent::PermissionRequest`). What is left without
///   one is the scripted `pending_responses` runtime's row
///   (`the_scripted_runtimes_rows_carry_no_resolver`), which no sweep can
///   settle and which is never withdrawn either — withdrawing without resolving
///   would strand the caller, strictly worse than a prompt the user answers.
/// - **An agent row's carried context is a SNAPSHOT where CC's is a live
///   closure.** `agentGetAppState` re-reads the parent store and re-applies the
///   transform on every call, so CC's recheck for an agent row sees a leader
///   grant made after the agent started. This port computes the agent context
///   once, at run start (`run_agent.rs:749-849`), and hands that same value to
///   the child query — so re-evaluating the row against it reproduces exactly
///   what the ASKING query itself would now decide, which is the property the
///   sweep needs. The cost is the conservative direction only: a leader's "don't
///   ask again" does not auto-approve an in-flight agent row. Closing it means
///   making the agent's whole permission context live, which is a different
///   (and larger) divergence than this one.
/// - Rows relayed to an out-of-process worker's leader
///   (`mailbox_response_target`) carry no context: the asker is in another
///   process. They also carry no `responder`, so they never reach the
///   evaluation below.
///
/// Returns `true` when the entry was resolved (CC's `decisionMade = true`).
pub async fn recheck_permission(
    entry: &ToolUseConfirm,
    app_store: &crate::state::store::AppStore,
    set_tool_use_confirm_queue: Option<
        crate::utils::swarm::leader_permission_bridge::SetToolUseConfirmQueueFn,
    >,
) -> bool {
    // An entry with no resolver has nothing this function could settle.
    if !entry.responder.is_some() {
        return false;
    }
    // CC `:205` `if (isResolved()) return` — the cheap pre-check before the
    // await; the binding claim is taken below, after it.
    if entry.responder.is_resolved() {
        return false;
    }
    let state = app_store.get();
    let request = &entry.request;
    // CC `:206-212` passes `ctx.toolUseContext` — the ASKER's context — into
    // `hasPermissionsToUseTool`, which reads `getAppState().toolPermissionContext`
    // off it (`permissions.ts:1167,1171,1184`). A row that recorded its asker's
    // context is re-evaluated against that; a row with none was raised by the
    // REPL for its own query, whose context IS the live one.
    let asking_context = entry
        .asking_tool_permission_context
        .as_ref()
        .unwrap_or_else(|| state.tool_permission_context.as_ref());
    let fresh = crate::utils::permissions::permissions::has_permissions_to_use_tool_async(
        crate::utils::permissions::permissions::HasPermissionsToUseToolParams {
            tool_use_id: &request.tool_use_id,
            tool_name: &request.tool_name,
            mcp_info: request.mcp_info.as_ref(),
            input_summary: &request.input_summary,
            input: &request.input,
            context: asking_context,
            messages: &[],
            app_store: Some(app_store),
            local_denial_tracking: None,
            abort_signal: None,
        },
    )
    .await;
    // CC `:213` / `:314` `if (freshResult.behavior === 'allow')`.
    let crate::utils::permissions::permissions::HasPermissionsToUseToolResult::Allow {
        updated_input,
        decision_reason,
        ..
    } = fresh
    else {
        return false;
    };
    // CC `:222` `if (!claim()) return` (the teammate copy's `:315`
    // `decisionMade = true`) — the atomic check-and-mark, taken BEFORE anything
    // is resolved or withdrawn, because the async
    // `has_permissions_to_use_tool_async` above opens a window in which the
    // user's dialog answer can arrive. The loser does nothing at all: it does
    // not resolve, and it does not withdraw the row the winner owns.
    if !entry.responder.claim() {
        return false;
    }
    let mut response = PermissionPromptResponse::new(PermissionPromptChoice::AllowOnce);
    match entry.source {
        // CC `:229` `ctx.buildAllow(freshResult.updatedInput ?? ctx.input)` —
        // the fresh value when there is one (`None` here IS `?? ctx.input`,
        // which `PermissionPromptResponse::apply_to_request` performs), and no
        // `decisionReason`, because `buildAllow` was called without `opts`.
        crate::types::permissions::PermissionRowSource::Interactive => {
            response.updated_input = updated_input;
        }
        // CC `:324-328` `{ ...freshResult, updatedInput: input, userModified:
        // false }` — the explicit key overrides the spread, so the ORIGINAL
        // input is what the teammate's tool re-runs on, whatever the permission
        // engine rewrote it to; the spread's `decisionReason` survives.
        crate::types::permissions::PermissionRowSource::InProcessTeammate => {
            response.updated_input = None;
            response.decision_reason = decision_reason;
        }
    }
    // CC's recheck resolves the PROMISE (`resolveOnce(ctx.buildAllow(...))` /
    // `resolve({...freshResult, ...})`), not the dialog's `onAllow`, so it
    // carries no `permissionUpdates` — the rules that made it allow are already
    // in the context. The explicit-empty marker is how this port says
    // "permissionUpdates was passed, and it was `[]`", which keeps
    // `apply_prompt_response` from substituting the choice's default updates
    // (`plan_mode_updates_for_choice`, a `setMode` for Enter/ExitPlanMode).
    response.permission_updates_explicit = true;
    // CC `:227` `ctx.removeFromQueue()` / `:321-323`, in CC's order: the
    // withdrawal comes after the claim and BEFORE `resolveOnce`, and it is
    // unconditional once claimed. Withdrawing only on a successful delivery
    // would leave a claimed row in the dialog when the waiter is already gone,
    // and a claimed row can no longer be dismissed by the user.
    if let Some(setter) = set_tool_use_confirm_queue {
        setter.remove_from_queue(entry.tool_use_id());
    }
    // CC `:229` `resolveOnce(ctx.buildAllow(...))`. The decision is made at the
    // claim (CC `:315` `decisionMade = true`), so the return value reports the
    // claim, not whether a waiter was still there to receive it.
    entry.responder.respond(response);
    true
}

/// Maps to CC `useCanUseTool.tsx:138-143` computing `await tool.description(input, ...)`
/// before handing the request to the swarm relay and the dialog. An
/// already-filled description stands (a hook or relay may have supplied one).
///
/// The classifier no longer writes here: CC's denial-limit fallback returns
/// `{...result, decisionReason}` (`permissions.ts:1050-1057`), leaving the
/// dialog's `description` to be exactly `tool.description(...)`, and the warning
/// reaches the user through `PermissionRuleExplanation`'s classifier arm.
pub fn fill_tool_description(request: &mut PermissionRequest) {
    if !request.description.is_empty() {
        return;
    }
    let Some(tool) = crate::services::tools::tool_execution::find_tool_call(&request.tool_name)
    else {
        return;
    };
    request.description = tool.description(&request.input);
}

/// Maps to CC `interactiveHandler.ts` channel relay guard
/// `!ctx.tool.requiresUserInteraction?.()`.
pub fn skips_channel_permission_relay_for_user_interaction(tool_name: &str) -> bool {
    match tool_name {
        // Maps to CC `AskUserQuestionTool.requiresUserInteraction()`: always true.
        "AskUserQuestion" => true,
        // Maps to CC `ExitPlanModeV2Tool.requiresUserInteraction()`: teammates
        // do not need local approval; non-teammates do.
        "ExitPlanMode" => !crate::utils::teammate::is_teammate(),
        // Official ReviewArtifact is not yet ported as a Rust tool, but the
        // channel relay guard still mirrors its CC requires-user-interaction
        // semantics if a dynamic/compat request reaches this handler.
        "ReviewArtifact" => true,
        _ => false,
    }
}

/// Maps to CC `interactiveHandler.ts` `channelUnsubscribe` lifecycle around
/// `channelCallbacks.onResponse(...)`.
pub struct ChannelPermissionRelayRegistration {
    pub request_id: String,
    pub receiver: async_channel::Receiver<PermissionPromptResponse>,
    unsubscribe: Option<crate::services::mcp::channel_permissions::ChannelPermissionUnsubscribe>,
}

impl ChannelPermissionRelayRegistration {
    pub fn unsubscribe(&mut self) {
        if let Some(unsubscribe) = self.unsubscribe.take() {
            unsubscribe.unsubscribe();
        }
    }
}

impl Drop for ChannelPermissionRelayRegistration {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

fn register_channel_permission_response(
    callbacks: &crate::services::mcp::channel_permissions::ChannelPermissionCallbacks,
    request_id: &str,
) -> ChannelPermissionRelayRegistration {
    let (tx, rx) = async_channel::bounded(1);
    let unsubscribe = callbacks.on_response(request_id, move |response| {
        let choice = match response.behavior {
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow => {
                PermissionPromptChoice::AllowOnce
            }
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Deny => {
                PermissionPromptChoice::Deny
            }
        };
        let _ = tx.try_send(PermissionPromptResponse::new(choice));
    });
    ChannelPermissionRelayRegistration {
        request_id: request_id.to_string(),
        receiver: rx,
        unsubscribe: Some(unsubscribe),
    }
}

/// Maps to CC `interactiveHandler.ts:315-405` channel permission relay
/// block: the relay clients among `getAppState().mcp.clients` each get a
/// fire-and-forget `permission_request` notification, then
/// `channelCallbacks.onResponse(...)` subscribes so the remote structured
/// reply can race the local dialog. With no relay client, nothing is
/// subscribed.
pub fn start_channel_permission_relay(
    context: &crate::tool::ToolUseContext,
    request: &PermissionRequest,
) -> Option<ChannelPermissionRelayRegistration> {
    use crate::services::mcp::channel_permissions;
    use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

    // CC `:316-319`: `feature('KAIROS') || feature('KAIROS_CHANNELS')`, and
    // callbacks in AppState, which the relay gate decided at seeding.
    if !(feature_enabled(FeatureFlag::Kairos) || feature_enabled(FeatureFlag::KairosChannels)) {
        return None;
    }
    let callbacks = context.channel_permission_callbacks.as_ref()?;
    if skips_channel_permission_relay_for_user_interaction(&request.tool_name) {
        return None;
    }
    let allowed_channels = crate::bootstrap::state::get_allowed_channels();
    let app_state = context.get_app_state();
    let channel_clients = channel_permissions::filter_permission_relay_clients(
        app_state
            .as_ref()
            .map_or(&[][..], |state| &state.mcp.clients),
        |name| {
            crate::services::mcp::channel_notification::find_channel_entry(name, &allowed_channels)
                .is_some()
        },
    );
    if channel_clients.is_empty() {
        return None;
    }

    let params = channel_permissions::channel_permission_request_params(
        &request.tool_use_id,
        &request.tool_name,
        &request.description,
        &request.input,
    );
    // Subscribed before any send goes out: CC sends and subscribes in one
    // synchronous stretch, so no reply can arrive in between; the sends
    // here run on another thread.
    let registration = register_channel_permission_response(callbacks, &params.request_id);
    // CC `:343-353`: `void client.client.notification(...).catch(...)`.
    if let (Ok(params), Some(handle)) = (
        serde_json::to_value(&params),
        crate::utils::process_runtime::runtime_handle_for_detached_work(),
    ) {
        for client in channel_clients {
            let (name, client) = (client.name.clone(), client.client.clone());
            let params = params.clone();
            handle.spawn(async move {
                if let Err(error) = client
                    .notification(
                        crate::services::mcp::channel_notification::CHANNEL_PERMISSION_REQUEST_METHOD,
                        params,
                    )
                    .await
                {
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!("Channel permission_request failed for {name}: {error}"),
                        crate::utils::debug::DebugLogLevel::Error,
                    );
                }
            });
        }
    }
    Some(registration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRuleValue};

    fn request_for_tool(tool_name: &str, tool_use_id: &str) -> PermissionRequest {
        PermissionRequest {
            permission_result: None,
            id: format!("perm-{tool_use_id}"),
            tool_use_id: tool_use_id.to_string(),
            tool_name: tool_name.to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Run command?".to_string(),
            message: String::new(),
            input_summary: "echo permission-gated".to_string(),
            input: serde_json::json!({ "command": "echo permission-gated" }),
            call_input: None,
            rule: PermissionRuleValue::new(tool_name, Some("echo permission-gated".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn bash_request(tool_use_id: &str) -> PermissionRequest {
        request_for_tool("Bash", tool_use_id)
    }

    /// `echo` is semantically neutral / read-only
    /// (`bash_tool/mod.rs#BASH_SEMANTIC_NEUTRAL_COMMANDS`), so
    /// `has_permissions_to_use_tool` allows it outright — useless for a test
    /// about a row that is WAITING on a decision. This one really asks.
    fn asking_bash_request(tool_use_id: &str) -> PermissionRequest {
        let mut request = request_for_tool("Bash", tool_use_id);
        request.input_summary = "cargo build".to_string();
        request.input = serde_json::json!({ "command": "cargo build" });
        request.rule = PermissionRuleValue::new("Bash", Some("cargo build".to_string()));
        request
    }

    #[test]
    fn interactive_handler_pushes_permission_request() {
        let mut queue = Vec::new();
        handle_interactive_permission(&mut queue, bash_request("toolu"));

        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].tool_use_id(), "toolu");
    }

    /// `handle_interactive_permission` is the queue-only push, and its one
    /// remaining caller is the scripted `use_can_use_tool` path
    /// (`hooks/use_can_use_tool.rs:752`) whose rows are answered by the REPL's
    /// `pending_responses` runtime, not by any pending promise. Those rows have
    /// no second racer, which is why [`PermissionPromptResponder::claim`]
    /// reports a win for them.
    ///
    /// The REPL's OWN query rows are no longer built here: `screens/repl.rs`
    /// attaches a `PermissionResponseDelivery::Command` resolver at
    /// `QueryEvent::PermissionRequest`, which is CC's per-entry `resolve`
    /// (`useCanUseTool.tsx:70`) with the actor round trip as its transport.
    #[test]
    fn the_scripted_runtimes_rows_carry_no_resolver() {
        let mut queue = Vec::new();
        handle_interactive_permission(&mut queue, bash_request("toolu"));

        assert!(!queue[0].responder.is_some());
        // No resolver means no racer: the answer path may proceed.
        assert!(queue[0].responder.claim());
    }

    /// Maps to: CC `PermissionContext.ts:88-92` `claim()` — "Atomically
    /// check-and-mark as resolved. Returns true if this caller won the race
    /// (nobody else has resolved yet), false otherwise."
    ///
    /// OLD SHAPE: `PermissionPromptResponder` was a bare
    /// `Option<Sender>` with no claim at all, so every racer had to infer
    /// winning from a `try_send` that had already delivered — after its side
    /// effects.
    #[test]
    fn the_claim_is_taken_once_and_a_clone_of_the_row_sees_it() {
        let (response_tx, _response_rx) = async_channel::bounded(1);
        let entry = ToolUseConfirm::new(bash_request("toolu_claim")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
        );
        // CC's racers close over one `createResolveOnce`; this port's racers
        // each hold a clone of the row.
        let sweep_view = entry.clone();

        assert!(!entry.responder.is_resolved());
        assert!(entry.responder.claim());
        assert!(entry.responder.is_resolved());
        assert!(!sweep_view.responder.claim(), "a second claim must lose");
        assert!(sweep_view.responder.is_resolved());
    }

    /// Maps to: CC `PermissionContext.ts:79-84` `resolve(value)` —
    /// `if (delivered) return`, then `claimed = true`.
    #[test]
    fn responding_delivers_once_and_closes_the_claim() {
        let (response_tx, response_rx) = async_channel::bounded(1);
        let entry = ToolUseConfirm::new(bash_request("toolu_resolve")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
        );

        assert!(
            entry
                .responder
                .respond(PermissionPromptResponse::new(PermissionPromptChoice::Deny))
        );
        assert!(
            entry.responder.is_resolved(),
            "CC's `resolve` sets `claimed = true` as well as `delivered`"
        );
        assert!(
            !entry.responder.respond(PermissionPromptResponse::new(
                PermissionPromptChoice::AllowOnce
            )),
            "CC `if (delivered) return`"
        );
        assert_eq!(
            response_rx.try_recv().expect("one delivery").choice,
            PermissionPromptChoice::Deny
        );
        assert!(response_rx.try_recv().is_err(), "and only one");
    }

    #[test]
    fn sink_queues_one_tool_use_confirm_and_resolves_the_waiting_call() {
        // Maps to: CC `useCanUseTool.tsx:307-324` — `handleInteractivePermission`
        // pushes the entry and the promise stays pending until `onAllow` fires.
        // The queued value is the ordinary `ToolUseConfirm`
        // (`PermissionRequest.tsx:137-166`) in the ordinary queue
        // (`REPL.tsx:1529`); only the resolver behind it differs.
        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let sink = create_repl_interactive_permission_sink(leader.setter.clone());
        let versions = leader.versions.clone();
        let answerer = std::thread::spawn(move || {
            // What the REPL does: apply the updater to `permission_queue`,
            // render it, then answer the entry that was queued.
            let queue = versions.recv_blocking().expect("entry must be queued");
            queue[0].responder.respond(PermissionPromptResponse::new(
                PermissionPromptChoice::AllowOnce,
            ));
            queue[0].clone()
        });

        let response = futures::executor::block_on(sink.ask(
            crate::tool::InteractivePermissionAsk::new(bash_request("toolu")),
        ))
        .expect("an answered prompt resolves the waiting call");

        let queued = answerer.join().expect("answerer panicked");
        assert_eq!(queued.tool_use_id(), "toolu");
        assert!(queued.responder.is_some());
        assert!(queued.permission_prompt_start_time_ms.is_some());
        assert_eq!(queued.request.description, "Run command?");
        assert_eq!(response.choice, PermissionPromptChoice::AllowOnce);
    }

    #[test]
    fn sink_ask_returns_none_when_the_repl_is_gone() {
        // The dropped-queue rule `repl_sandbox_ask_flow` uses: with no resolver
        // left, the caller falls back to its no-prompt behaviour instead of
        // parking on a promise nothing can settle. An unmounted REPL is an
        // applier whose queue goes nowhere.
        let sink = create_repl_interactive_permission_sink(
            crate::utils::swarm::leader_permission_bridge::SetToolUseConfirmQueueFn::new(
                |updater| {
                    drop(updater(Vec::new()));
                },
            ),
        );

        assert!(
            futures::executor::block_on(sink.ask(crate::tool::InteractivePermissionAsk::new(
                bash_request("toolu")
            )))
            .is_none()
        );
    }

    /// Maps to: CC `inProcessRunner.ts:263-281` — the row an in-process
    /// teammate's `createInProcessCanUseTool` builds is the ONLY one whose
    /// write-back passes `{ preserveMode: true }`. CC's discriminator is which
    /// closure built it; this port's is the teammate task scope, read where the
    /// row is built.
    ///
    /// OLD SHAPE: no field existed, so the leader applied a teammate's answer —
    /// `setMode` and all — with `PermissionContext.ts:139-147` semantics, and a
    /// teammate's "auto-accept edits" moved the coordinator's own mode.
    #[test]
    fn only_an_in_process_teammates_row_asks_the_leader_to_preserve_its_mode() {
        let leader_row = tool_use_confirm_for_request(
            bash_request("toolu_leader"),
            crate::types::permissions::PermissionPromptResponder::default(),
            None,
        );
        assert_eq!(
            leader_row.source,
            crate::types::permissions::PermissionRowSource::Interactive
        );
        assert!(!leader_row.preserves_leader_permission_mode());

        let teammate_row =
            futures::executor::block_on(crate::utils::teammate_context::run_with_teammate_context(
                crate::utils::teammate_context::TeammateContext {
                    agent_id: "agent-1".to_string(),
                    agent_name: "reviewer".to_string(),
                    team_name: "team".to_string(),
                    color: Some("green".to_string()),
                    plan_mode_required: false,
                    parent_session_id: "session".to_string(),
                    is_in_process: true,
                    abort_controller: crate::tool::AbortController::default(),
                },
                async {
                    let mut request = bash_request("toolu_teammate");
                    request.permission_result =
                        Some(crate::types::permissions::PermissionDecision::Ask {
                            message: "ask".into(),
                            updated_input: Some(
                                serde_json::json!({"command":"override-must-not-replace-original"}),
                            ),
                            decision_reason: None,
                            suggestions: Vec::new(),
                            blocked_path: None,
                            metadata: None,
                            is_bash_security_check_for_misparsing: false,
                            pending_classifier_check: None,
                            content_blocks: Vec::new(),
                        });
                    tool_use_confirm_for_request(
                        request,
                        crate::types::permissions::PermissionPromptResponder::default(),
                        None,
                    )
                },
            ));
        assert_eq!(
            teammate_row.source,
            crate::types::permissions::PermissionRowSource::InProcessTeammate
        );
        assert!(teammate_row.preserves_leader_permission_mode());
        assert_eq!(
            teammate_row.request.input,
            bash_request("toolu_teammate").input
        );
    }

    /// Maps to: CC `interactiveHandler.ts:204-231` / `inProcessRunner.ts:305-330`
    /// `recheckPermission()` — re-run `hasPermissionsToUseTool`, and on `allow`
    /// resolve the pending promise and withdraw the row.
    ///
    /// OLD SHAPE: no such body existed; the entry had no recheck to call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recheck_resolves_and_withdraws_only_once_the_rules_allow() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let (response_tx, response_rx) = async_channel::bounded(1);
        let entry = ToolUseConfirm::new(asking_bash_request("toolu_recheck")).with_responder(
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
        );
        leader.setter.push_to_queue(entry.clone());

        // Still an ask: nothing to resolve, nothing withdrawn.
        assert!(!recheck_permission(&entry, &store, Some(leader.setter.clone())).await);
        assert_eq!(leader.queue.lock().unwrap().len(), 1);
        assert!(response_rx.try_recv().is_err());

        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        store.set_tool_permission_context(granted);

        assert!(recheck_permission(&entry, &store, Some(leader.setter.clone())).await);
        assert_eq!(
            response_rx.try_recv().expect("resolved").choice,
            PermissionPromptChoice::AllowOnce
        );
        assert!(leader.queue.lock().unwrap().is_empty());
    }

    /// A leader whose live context allows every Bash use — what "yes, don't ask
    /// again" on some other prompt leaves behind, and the state that makes a
    /// recheck reading the LEADER resolve.
    fn leader_store_allowing_bash() -> crate::state::store::AppStore {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        store.set_tool_permission_context(granted);
        store
    }

    /// The permission context an agent given `allowedTools: []` runs under.
    ///
    /// Maps to: CC `runAgent.ts:469-478` — `agentGetAppState` REPLACES
    /// `alwaysAllowRules` with `{ cliArg: <parent's cliArg>, session:
    /// [...allowedTools] }` whenever `allowedTools !== undefined`, dropping the
    /// parent's user/project/local/flag/policy/command/session allows
    /// (`run_agent.rs#agent_permission_context_for_run`, same branch).
    fn agent_context_scoped_to_no_allow_rules() -> crate::tool::ToolPermissionContext {
        crate::tool::ToolPermissionContext {
            always_allow_rules: std::collections::HashMap::new(),
            ..crate::tool::ToolPermissionContext::default()
        }
    }

    /// Maps to: CC `interactiveHandler.ts:206-212` / `inProcessRunner.ts:307-313`
    /// — `recheckPermission` re-runs `hasPermissionsToUseTool(ctx.tool,
    /// ctx.input, ctx.toolUseContext, ...)` with the ASKER's `toolUseContext`,
    /// whose `getAppState()` for a subagent is `agentGetAppState`
    /// (`runAgent.ts:416-497`), not the leader's plain store read.
    ///
    /// The row here is a subagent's, scoped by `allowedTools` so its own
    /// context has NO allow rules, while the leader's live context allows all
    /// Bash. CC re-evaluates the agent's context and keeps asking.
    ///
    /// OLD SHAPE: the entry carried no context at all, so the sweep evaluated
    /// `app_store.get().tool_permission_context` for every row. This test
    /// FAILED on all four assertions: the recheck returned `true`, withdrew the
    /// row from the leader's queue, and resolved the subagent's pending
    /// `canUseTool` with an `AllowOnce` its own permission context had never
    /// produced — an invented allow, which is what #218 booked and #179 had
    /// wrongly called impossible.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recheck_asks_the_rows_own_context_so_a_leader_grant_cannot_invent_an_agent_allow() {
        let store = leader_store_allowing_bash();
        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let (response_tx, response_rx) = async_channel::bounded(1);
        let entry = tool_use_confirm_for_request(
            asking_bash_request("toolu_agent"),
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
            Some(agent_context_scoped_to_no_allow_rules()),
        );
        leader.setter.push_to_queue(entry.clone());

        assert!(
            !recheck_permission(&entry, &store, Some(leader.setter.clone())).await,
            "the asking agent's own context still asks, so there is no decision"
        );
        assert_eq!(
            leader.queue.lock().unwrap().len(),
            1,
            "an unresolved row is not withdrawn"
        );
        assert!(
            response_rx.try_recv().is_err(),
            "the subagent's pending canUseTool must not be resolved"
        );
        assert!(!entry.responder.is_resolved());
    }

    /// The other direction of the same carrier, so the test above cannot pass
    /// by the recheck simply never resolving: here the LEADER would still ask
    /// and the ASKER's context allows, and CC resolves — because
    /// `hasPermissionsToUseTool` reads the asker's context, whichever way the
    /// two differ (`runAgent.ts:421-434` narrows the mode for a `plan` agent
    /// under a `default` leader; `:469-478` can leave an agent with allow rules
    /// the parent lacks, e.g. an SDK `--allowedTools` list reaching `session`).
    ///
    /// OLD SHAPE: the leader's context was the only one consulted, so this row
    /// was left asking — the miss half of the same bug.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recheck_resolves_when_the_rows_own_context_allows_and_the_leader_would_not() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut agent_context = crate::tool::ToolPermissionContext::default();
        agent_context.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let (response_tx, response_rx) = async_channel::bounded(1);
        let entry = tool_use_confirm_for_request(
            asking_bash_request("toolu_agent_allowed"),
            crate::types::permissions::PermissionPromptResponder::new(response_tx),
            Some(agent_context),
        );
        leader.setter.push_to_queue(entry.clone());

        assert!(recheck_permission(&entry, &store, Some(leader.setter.clone())).await);
        assert_eq!(
            response_rx.try_recv().expect("resolved").choice,
            PermissionPromptChoice::AllowOnce
        );
        assert!(leader.queue.lock().unwrap().is_empty());
    }

    /// Maps to: the ONE line where CC's two `recheckPermission` copies differ.
    ///
    /// ```ts
    /// // interactiveHandler.ts:229
    /// resolveOnce(ctx.buildAllow(freshResult.updatedInput ?? ctx.input))
    /// // inProcessRunner.ts:324-328
    /// resolve({ ...freshResult, updatedInput: input, userModified: false })
    /// ```
    ///
    /// The spread runs FIRST, so `updatedInput: input` overrides
    /// `freshResult.updatedInput` unconditionally — a teammate's tool re-runs on
    /// the model's ORIGINAL input — while `decisionReason` survives the spread
    /// and is absent from the interactive copy (`buildAllow` with no `opts`,
    /// `PermissionContext.ts:264-284`).
    ///
    /// The fresh evaluation here really does rewrite the input: an exact-match
    /// Bash allow rule returns `updated_input: Some({command: <trimmed>})`
    /// (`bash_permissions.rs:687-695`), so the untrimmed command below makes the
    /// two policies observably different values rather than two spellings of
    /// the same one.
    ///
    /// OLD SHAPE: one merged body took `interactiveHandler.ts:229`'s nullish
    /// fallback for both classes and cited "`:229` / `:326`" as if the two CC
    /// sites agreed. The teammate assertions below both failed: `updated_input`
    /// came back as the rewritten command, and `decision_reason` came back
    /// `None`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_teammate_rows_recheck_re_runs_the_original_input_and_keeps_the_fresh_reason() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new(
                "Bash",
                Some("cargo build".to_string()),
            )],
        );
        store.set_tool_permission_context(granted);

        // The model's own input, as the row carries it: not yet trimmed, which
        // is what the permission engine rewrites.
        let untrimmed = |tool_use_id: &str| {
            let mut request = request_for_tool("Bash", tool_use_id);
            request.input_summary = "  cargo build  ".to_string();
            request.input = serde_json::json!({ "command": "  cargo build  " });
            request.rule = PermissionRuleValue::new("Bash", Some("  cargo build  ".to_string()));
            request
        };

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let (interactive_tx, interactive_rx) = async_channel::bounded(1);
        let interactive = tool_use_confirm_for_request(
            untrimmed("toolu_interactive"),
            crate::types::permissions::PermissionPromptResponder::new(interactive_tx),
            None,
        );
        assert!(recheck_permission(&interactive, &store, Some(leader.setter.clone())).await);
        let interactive_response = interactive_rx.try_recv().expect("resolved");
        assert_eq!(
            interactive_response.updated_input,
            Some(serde_json::json!({ "command": "cargo build" })),
            "CC `:229` takes `freshResult.updatedInput` when there is one"
        );
        assert!(
            interactive_response.decision_reason.is_none(),
            "CC `:229` calls `ctx.buildAllow(input)` with no `opts`, so no decisionReason"
        );

        let (teammate_tx, teammate_rx) = async_channel::bounded(1);
        let mut teammate = tool_use_confirm_for_request(
            untrimmed("toolu_teammate"),
            crate::types::permissions::PermissionPromptResponder::new(teammate_tx),
            None,
        );
        teammate.source = crate::types::permissions::PermissionRowSource::InProcessTeammate;
        assert!(recheck_permission(&teammate, &store, Some(leader.setter.clone())).await);
        let teammate_response = teammate_rx.try_recv().expect("resolved");
        assert!(
            teammate_response.updated_input.is_none(),
            "CC `:326` `updatedInput: input` overrides the spread, and `None` here \
             IS the row's original input (`apply_to_request`)"
        );
        assert_eq!(
            teammate_response
                .apply_to_request(teammate.request.clone())
                .input,
            serde_json::json!({ "command": "  cargo build  " })
        );
        assert!(
            teammate_response.decision_reason.is_some(),
            "CC `:325` `...freshResult` carries `decisionReason` through"
        );
    }

    /// A row with no resolver at all belongs to the scripted
    /// `pending_responses` runtime (`the_scripted_runtimes_rows_carry_no_resolver`),
    /// which has no pending promise for a sweep to settle. Recheck must leave it
    /// alone — withdrawing it without resolving would strand its caller.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recheck_leaves_a_resolverless_row_in_the_queue() {
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut granted = store.tool_permission_context();
        granted.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Session,
            vec![PermissionRuleValue::new("Bash", None)],
        );
        store.set_tool_permission_context(granted);

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let entry = ToolUseConfirm::new(asking_bash_request("toolu_repl_owned"));
        leader.setter.push_to_queue(entry.clone());

        assert!(!recheck_permission(&entry, &store, Some(leader.setter.clone())).await);
        assert_eq!(leader.queue.lock().unwrap().len(), 1);
    }

    #[test]
    fn channel_permission_relay_guard_matches_official_requires_user_interaction() {
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate::clear_dynamic_team_context();
        assert!(!skips_channel_permission_relay_for_user_interaction("Bash"));
        assert!(skips_channel_permission_relay_for_user_interaction(
            "AskUserQuestion"
        ));
        assert!(skips_channel_permission_relay_for_user_interaction(
            "ReviewArtifact"
        ));
        assert!(skips_channel_permission_relay_for_user_interaction(
            "ExitPlanMode"
        ));

        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "agent-1".to_string(),
                agent_name: "Planner".to_string(),
                team_name: "team".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        assert!(!skips_channel_permission_relay_for_user_interaction(
            "ExitPlanMode"
        ));
        crate::utils::teammate::clear_dynamic_team_context();
    }

    #[test]
    fn channel_permission_response_registration_maps_allow_and_deny_to_prompt_responses() {
        let callbacks =
            crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default();
        let mut allow_registration = register_channel_permission_response(&callbacks, "abcde");
        assert!(callbacks.resolve(
            "ABCDE",
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow,
            "plugin:telegram:tg",
        ));
        assert_eq!(
            allow_registration.receiver.try_recv().unwrap().choice,
            PermissionPromptChoice::AllowOnce
        );
        allow_registration.unsubscribe();

        let _deny_registration = register_channel_permission_response(&callbacks, "fghij");
        assert!(callbacks.resolve(
            "fghij",
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Deny,
            "plugin:telegram:tg",
        ));
        assert!(!callbacks.resolve(
            "fghij",
            crate::services::mcp::channel_permissions::ChannelPermissionBehavior::Allow,
            "plugin:telegram:tg",
        ));
    }

    #[test]
    fn start_channel_permission_relay_noops_when_official_gate_disabled() {
        let callbacks =
            crate::services::mcp::channel_permissions::ChannelPermissionCallbacks::default();
        let context = crate::tool::ToolUseContext::default()
            .with_channel_permission_callbacks(Some(callbacks));
        assert!(start_channel_permission_relay(&context, &bash_request("toolu_123")).is_none());
        assert!(
            start_channel_permission_relay(
                &context,
                &request_for_tool("AskUserQuestion", "toolu_question"),
            )
            .is_none()
        );
    }
    #[test]
    fn queue_display_input_matches_official_ask_updated_input_and_keeps_call_transport() {
        let mut request = bash_request("toolu-updated");
        let call_input = serde_json::json!({"command":"validated-original", "privateCarrier":true});
        request.call_input = Some(call_input.clone());
        let updated = serde_json::json!({"command":"tool-provided-update"});
        request.permission_result = Some(crate::types::permissions::PermissionDecision::Ask {
            message: "review update".into(),
            updated_input: Some(updated.clone()),
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        });
        let mut queue = Vec::new();
        handle_interactive_permission(&mut queue, request);
        assert_eq!(queue[0].request.input, updated);
        assert_eq!(queue[0].request.call_input, Some(call_input));
    }
}
