//! In-process teammate runner.
//!
//! Maps to: CC `utils/swarm/inProcessRunner.ts`.
//!
//! This Rust slice runs teammate prompts through the official
//! `AgentTool/runAgent.ts` boundary, mirrors messages into
//! `InProcessTeammateTaskState`, sends official idle notifications, and keeps
//! idle teammates alive by polling the official teammate mailbox boundary for
//! wake-up/shutdown messages. Live per-message progress is mirrored through
//! the `runAgent` message-yield seam, and the prompt loop mirrors the official
//! in-process autocompaction/reset branch. Teammate permission asks (#156)
//! resolve on the path the child query actually reaches
//! (`run_agent.rs#ask_parent_for_agent_permission`): the leader's dialog with
//! a worker badge through the inherited `interactive_permission_sink` — wrapped
//! per turn by [`teammate_leader_dialog_sink`], which carries CC's abort race
//! and permission-wait report (CC `createInProcessCanUseTool`'s standard leg) —
//! or [`resolve_teammate_ask_via_mailbox`] (CC's mailbox fallback leg) when no
//! sink is installed.

use crate::constants::xml::TEAMMATE_MESSAGE_TAG;
use crate::tasks::in_process_teammate_task::{
    TeammateIdentity, append_teammate_message, clear_teammate_current_work,
    fail_in_process_teammate_task, mark_teammate_idle, mark_teammate_running,
    pop_pending_user_message_from_teammate, replace_teammate_messages,
    update_teammate_progress_from_message,
};
use crate::tool::{AbortController, ToolUseContext};
use crate::tools::agent_tool::load_agents_dir::AgentDefinition;
use crate::types::message::{Message, UserContent, UserMessage};
use crate::types::permissions::PermissionMode;
use crate::utils::teammate_context::TeammateContext;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAILBOX_POLL_INTERVAL_MS: u64 = 500;

/// Maps to: CC `utils/swarm/inProcessRunner.ts:114`
/// `const PERMISSION_POLL_INTERVAL_MS = 500` — the `setInterval` period the
/// mailbox fallback leg scans the teammate's own mailbox on (`:386`, `:427`).
const PERMISSION_POLL_INTERVAL_MS: u64 = 500;

/// Maps to CC `utils/swarm/teammatePromptAddendum.ts#TEAMMATE_SYSTEM_PROMPT_ADDENDUM`.
pub const TEAMMATE_SYSTEM_PROMPT_ADDENDUM: &str = r#"
# Agent Teammate Communication

IMPORTANT: You are running as an agent in a team. To communicate with anyone on your team:
- Use the SendMessage tool with `to: "<name>"` to send messages to specific teammates
- Use the SendMessage tool with `to: "*"` sparingly for team-wide broadcasts

Just writing a response in text is not visible to others on your team - you MUST use the SendMessage tool.

The user interacts primarily with the team lead. Your work is coordinated through the task system and teammate messaging.
"#;

/// Maps to: CC `utils/swarm/inProcessRunner.ts#WaitResult`.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitResult {
    ShutdownRequest {
        request: serde_json::Value,
        original_message: String,
    },
    NewMessage {
        message: String,
        from: String,
        color: Option<String>,
        summary: Option<String>,
    },
    Aborted,
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#InProcessRunnerConfig`.
#[derive(Clone, Debug)]
pub struct InProcessRunnerConfig {
    pub identity: TeammateIdentity,
    pub task_id: String,
    pub prompt: String,
    pub description: Option<String>,
    pub model: Option<String>,
    /// Maps to CC `InProcessRunnerConfig.allowedTools`.
    pub allowed_tools: Option<Vec<String>>,
    pub agent_definition: Option<AgentDefinition>,
    pub teammate_context: TeammateContext,
    pub tool_use_context: ToolUseContext,
    pub abort_controller: AbortController,
    pub invoking_request_id: Option<String>,
}

/// Maps to CC `inProcessRunner.ts#formatAsTeammateMessage`.
pub fn format_as_teammate_message(
    from: &str,
    text: &str,
    color: Option<&str>,
    summary: Option<&str>,
) -> String {
    let color_attr = color
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" color=\"{value}\""))
        .unwrap_or_default();
    let summary_attr = summary
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" summary=\"{value}\""))
        .unwrap_or_default();
    format!(
        "<{TEAMMATE_MESSAGE_TAG} teammate_id=\"{from}\"{color_attr}{summary_attr}>\n{text}\n</{TEAMMATE_MESSAGE_TAG}>"
    )
}

fn teammate_user_message(content: String) -> Message {
    Message::User(UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![UserContent::Text(content)],
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

/// Maps to: CC `utils/swarm/inProcessRunner.ts:198-334` — the STANDARD branch
/// of `createInProcessCanUseTool`, taken when the leader's ToolUseConfirm queue
/// is available (`getLeaderToolUseConfirmQueue()` non-null; Rust: the context
/// carries an `interactive_permission_sink`).
///
/// The port's carrier for that branch is the sink itself, so this is a WRAPPER
/// installed by [`run_in_process_teammate`] on the context it hands to
/// `run_agent` — the same construction point and the same per-turn
/// `currentWorkAbortController` CC closes over at `:1179-1192`. The queueing and
/// the allow/reject answer stay in the leader's sink; what this adds is the two
/// things CC's promise wrapper owns and the sink cannot know about:
///
/// 1. **The abort race** (CC `:179-181`, `:191-193`, `:209-221`): CC checks
///    `abortController.signal.aborted` twice before showing UI and registers an
///    `'abort'` listener that resolves the pending ask with
///    `SUBAGENT_REJECT_MESSAGE`. Without it a teammate whose turn is aborted
///    (Escape on that teammate) stays parked on a channel only the leader's
///    dialog can answer, because `AbortController` here is a flag, not a
///    cancellation of the awaiting future.
/// 2. **The permission-wait report** (CC `:201`, `:205-207`): `permissionStartMs`
///    is stamped when the entry is queued and `reportPermissionWait()` fires on
///    EVERY resolution path (`:212` signal abort, `:247` dialog abort, `:262`
///    allow, `:299` reject, `:320` recheck) into `onPermissionWaitMs`, which the
///    runner turns into `totalPausedMs += waitMs` (`:1182-1191`).
///    `Instant::elapsed` is the monotonic form of `Date.now() - permissionStartMs`.
///
/// 3. **Entry withdrawal on abort** (CC `:210-216`): the `onAbortListener`
///    marks the decision made (`decisionMade = true`) and then ends with
///    `setToolUseConfirmQueue(queue => queue.filter(item => item.toolUseID !==
///    toolUseID))`. The teammate has stopped waiting, so the leader's dialog row
///    must go with it — otherwise the human answers a prompt for a tool use
///    nobody is listening to, and the answer is a silent no-op
///    (`PermissionPromptResponder::respond` returns false once the waiter is
///    gone) that nonetheless persisted its rules. Both halves travel one updater
///    (`claim_and_remove_from_queue`), because CC's two statements are adjacent
///    and synchronous while this port's updater is applied by the REPL later.
///    The updater that reaches a queued row from outside the REPL is
///    `leader_permission_bridge.rs#get_leader_tool_use_confirm_queue`, CC's own
///    `:195` accessor, read at call time exactly as CC reads it.
///
///    The claim cannot be taken HERE, synchronously, the way CC's `:211`
///    `decisionMade = true` is: the row's responder is created inside the
///    leader sink's per-ask closure
///    (`interactive_handler.rs#create_repl_interactive_permission_sink`) and
///    never comes back out — this site holds `tool_use_id` and the pending
///    `leader_sink.ask` future, nothing else. What restores CC's ORDER is the
///    answer side settling the queue FIFO before it addresses a row
///    (`screens/repl.rs#settle_pending_permission_queue_updaters`), so an abort
///    published before a keypress is applied before that keypress can claim.
///
/// Note which paths withdraw: CC filters in the SIGNAL listener (`:214-216`)
/// and in `recheckPermission` (`:321-323`), but NOT in the entry's own
/// `onAbort` (`:240-249`) — there the dialog is already removing itself.
///
/// `None` (abort, or a leader that dropped the entry) leaves the caller's deny
/// in place, which is CC's `{ behavior: 'ask', message: SUBAGENT_REJECT_MESSAGE }`
/// after `toolExecution.ts` turns it into the subagent reject copy.
fn teammate_leader_dialog_sink(
    leader_sink: crate::tool::InteractivePermissionSink,
    task_id: String,
    abort_controller: AbortController,
) -> crate::tool::InteractivePermissionSink {
    crate::tool::InteractivePermissionSink::new(
        move |ask: crate::tool::InteractivePermissionAsk| {
            let leader_sink = leader_sink.clone();
            let task_id = task_id.clone();
            let abort_controller = abort_controller.clone();
            Box::pin(async move {
                // CC `:179-181` / `:191-193`: aborted before the dialog is
                // shown → SUBAGENT reject without queueing anything, and no
                // wait to report (`reportPermissionWait` is not reached).
                if abort_controller.is_aborted() {
                    return None;
                }
                // CC `:231` `toolUseID` — the row this ask owns, and the key
                // the abort listener withdraws it by.
                let tool_use_id = ask.request.tool_use_id.clone();
                // CC `:201` `const permissionStartMs = Date.now()`.
                let permission_start = std::time::Instant::now();
                let mut abort_signal = abort_controller.signal();
                let response = tokio::select! {
                    // `biased` is CC's `decisionMade` guard: an answer that has
                    // already arrived is a decision made, and a simultaneous
                    // abort must not overwrite it. `select!` is otherwise random.
                    biased;
                    // CC `:230` `toolUseContext` travels to the leader's row
                    // untouched — this wrapper owns the abort race and the wait
                    // report, not the payload.
                    response = leader_sink.ask(ask) => response,
                    // CC `:209-221` `abortController.signal.addEventListener(
                    // 'abort', onAbortListener, { once: true })`.
                    _ = abort_signal.aborted() => {
                        // CC `:210-216` — `decisionMade = true` and then the
                        // filter, so a dialog answer still in flight loses its
                        // claim and stops before it persists anything.
                        if let Some(setter) =
                            crate::utils::swarm::leader_permission_bridge::get_leader_tool_use_confirm_queue()
                        {
                            setter.claim_and_remove_from_queue(&tool_use_id);
                        }
                        None
                    }
                };
                // CC `:205-207` `reportPermissionWait()`.
                crate::tasks::in_process_teammate_task::record_teammate_permission_wait_ms(
                    &task_id,
                    permission_start.elapsed().as_millis() as u64,
                );
                response
            })
        },
    )
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:337-447` — the MAILBOX FALLBACK
/// branch of `createInProcessCanUseTool`, taken when the leader's
/// ToolUseConfirm queue is unavailable (`getLeaderToolUseConfirmQueue()`
/// returns null; Rust: the context has no `interactive_permission_sink`).
///
/// #156 re-homed this from the deleted `create_in_process_can_use_tool`
/// callback: that callback fed `RunAgentInput.can_use_tool`, which #141
/// deliberately never read, so none of it ever ran. The live caller is
/// `run_agent.rs#ask_parent_for_agent_permission` (the path a teammate's ask
/// actually reaches). The evaluation half of the deleted callback
/// (`hasPermissionsToUseTool` + non-ask passthrough) is NOT re-homed — the
/// child query's own permission gate already runs it, which is the single
/// evaluation #141 established.
///
/// CC ordering preserved: register the response callback BEFORE sending
/// (`inProcessRunner.ts:350-352`), send via mailbox (`:383`), then poll the
/// teammate's own mailbox for the response (`:385-433`), resolving allow with
/// the leader's `updatedInput`/`permissionUpdates` (`:353-371`) and reject with
/// the leader's feedback (`:372-377`).
///
/// TIMING CONTRACT (CC `:386-433`, `:435-442`): `setInterval(..., 500)` with NO
/// deadline. The only two exits are the leader's answer and
/// `abortController.signal` — the interval body's own aborted check (`:388-392`)
/// and the `'abort'` listener (`:435-442`) both `cleanup()` (clearInterval +
/// `unregisterPermissionCallback` + removeEventListener) and resolve
/// `SUBAGENT_REJECT_MESSAGE`. A bounded wait used to live here
/// (`COMETIX_SWARM_PERMISSION_WAIT_MS`, 1 s default) justified as "the same
/// contract as `swarm_worker_handler.rs#try_resolve_swarm_worker_ask`"; that is
/// a Rust-internal consistency argument, not a CC anchor — CC's
/// `swarmWorkerHandler.ts:67-147` is also deadline-free, waiting on the
/// registered callback with abort as its only early exit. Removed.
///
/// `reportPermissionWait` is deliberately NOT called on this leg: CC's five
/// `onPermissionWaitMs` sites (`:212`, `:247`, `:262`, `:299`, `:320`) all sit
/// inside the `setToolUseConfirmQueue` branch (`:198-334`). A teammate parked on
/// the mailbox does not discount its elapsed time.
///
/// The deleted `create_in_process_can_use_tool` callback's `pendingWorkerRequest`
/// app-state writes are NOT carried over — CC's `createInProcessCanUseTool`
/// never writes `pendingWorkerRequest` (zero hits in `inProcessRunner.ts`; that
/// state belongs to the out-of-process `swarmWorkerHandler.ts`).
///
/// `None` means unresolved (send failed / aborted); the caller keeps its
/// existing deny, which lands on the SUBAGENT reject copy the same way CC
/// resolves `SUBAGENT_REJECT_MESSAGE`.
pub(crate) async fn resolve_teammate_ask_via_mailbox(
    identity: &TeammateContext,
    request: &crate::types::permissions::PermissionRequest,
    abort_controller: &AbortController,
) -> Option<crate::types::permissions::PermissionPromptResponse> {
    resolve_teammate_ask_via_mailbox_with_interval(
        identity,
        request,
        abort_controller,
        Duration::from_millis(PERMISSION_POLL_INTERVAL_MS),
    )
    .await
}

/// [`resolve_teammate_ask_via_mailbox`] with CC's `PERMISSION_POLL_INTERVAL_MS`
/// supplied by the caller, so tests can drive the same loop without waiting out
/// the official 500 ms period — the split
/// `wait_for_next_prompt_or_shutdown_with_interval` already uses in this file.
async fn resolve_teammate_ask_via_mailbox_with_interval(
    identity: &TeammateContext,
    request: &crate::types::permissions::PermissionRequest,
    abort_controller: &AbortController,
    poll_interval: Duration,
) -> Option<crate::types::permissions::PermissionPromptResponse> {
    let Ok(swarm_request) = crate::utils::swarm::permission_sync::create_permission_request(
        crate::utils::swarm::permission_sync::CreatePermissionRequestParams {
            tool_name: request.tool_name.clone(),
            tool_use_id: request.tool_use_id.clone(),
            input: request.input.clone(),
            description: request.description.clone(),
            // CC `permissionSuggestions: result.suggestions` (:344).
            permission_suggestions:
                crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
                    &request.suggestions,
                ),
            team_name: Some(identity.team_name.clone()),
            worker_id: Some(identity.agent_id.clone()),
            worker_name: Some(identity.agent_name.clone()),
            worker_color: identity.color.clone(),
        },
    ) else {
        return None;
    };
    let request_id = swarm_request.id.clone();
    let original_input = request.input.clone();
    let response_slot = Arc::new(Mutex::new(
        None::<crate::types::permissions::PermissionPromptResponse>,
    ));
    let allow_slot = Arc::clone(&response_slot);
    let reject_slot = Arc::clone(&response_slot);

    // Register BEFORE send (CC inProcessRunner.ts:350-352 race avoidance).
    crate::hooks::use_swarm_permission_poller::register_permission_callback(
        crate::hooks::use_swarm_permission_poller::PermissionResponseCallback {
            request_id: request_id.clone(),
            tool_use_id: request.tool_use_id.clone(),
            // CC `onAllow(updatedInput, permissionUpdates, _feedback, ...)`
            // (:353-371): finalInput falls back to the original input when the
            // leader sent none; the updates ride the response explicitly.
            on_allow: Arc::new(move |allowed_input, updates, _feedback| {
                let final_input = match allowed_input {
                    Some(value) if value.as_object().is_some_and(|o| !o.is_empty()) => value,
                    _ => original_input.clone(),
                };
                *allow_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                    crate::types::permissions::PermissionPromptResponse::allow_once_with_input(
                        final_input,
                    )
                    .with_permission_updates(updates),
                );
            }),
            // CC `onReject(feedback, ...)` (:372-377): the feedback becomes
            // the SUBAGENT_REJECT_MESSAGE_WITH_REASON_PREFIX copy downstream
            // (`permission_terminal_result_for_decision`, is_subagent arm).
            on_reject: Arc::new(move |feedback| {
                let mut response = crate::types::permissions::PermissionPromptResponse::new(
                    crate::types::permissions::PermissionPromptChoice::Deny,
                );
                if let Some(feedback) = feedback {
                    response = response.with_feedback(feedback);
                }
                *reject_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(response);
            }),
        },
    );

    crate::utils::swarm::permission_sync::write_permission_request(swarm_request.clone());
    if !crate::utils::swarm::permission_sync::send_permission_request_via_mailbox(&swarm_request) {
        crate::hooks::use_swarm_permission_poller::unregister_permission_callback(&request_id);
        return None;
    }

    loop {
        // CC `:388-392` (interval body) and `:435-442` (abort listener): the
        // only non-answer exit. `cleanup()` unregisters the callback.
        if abort_controller.is_aborted() {
            crate::hooks::use_swarm_permission_poller::unregister_permission_callback(&request_id);
            return None;
        }
        // The registered callback already ran (CC resolves the promise from
        // inside `onAllow`/`onReject`, `:353-379`).
        if let Some(response) = response_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Some(response);
        }
        // CC `:394-425`: scan the teammate's own mailbox for an unread
        // `permission_response` carrying this request id, mark it read, and hand
        // it to `processMailboxPermissionResponse` — which invokes the callback
        // registered above. This is the leg's real wake source: the leader
        // answers a mailbox-target prompt through
        // `permission_sync::send_permission_response_via_mailbox`
        // (`repl.rs#send_mailbox_permission_prompt_response`, CC
        // `useInboxPoller.ts:382-389`), which writes exactly this message.
        scan_teammate_mailbox_for_permission_response(identity, &request_id);
        if let Some(response) = response_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Some(response);
        }
        // Port stand-in for CC's concurrently-running `useSwarmPermissionPoller`
        // interval (`useSwarmPermissionPoller.ts:322`, also 500 ms), which this
        // port has no React host for: it polls `pollForResponse` per registered
        // request and calls the same `processResponse`. Not a second CC branch
        // of `createInProcessCanUseTool` — the hook, folded into the only loop
        // that runs.
        if let Some(resolved_response) = crate::utils::swarm::permission_sync::poll_for_response(
            &request_id,
            Some(&identity.team_name),
        ) {
            let _ = crate::utils::swarm::permission_sync::delete_resolved_permission(
                &request_id,
                Some(&identity.team_name),
            );
            let _ = crate::hooks::use_swarm_permission_poller::process_permission_response(
                &resolved_response,
            );
            if let Some(response) = response_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                return Some(response);
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:394-425` — the body of the
/// mailbox fallback's poll interval.
///
/// ```ts
/// const allMessages = await readMailbox(identity.agentName, identity.teamName)
/// for (let i = 0; i < allMessages.length; i++) {
///   const msg = allMessages[i]
///   if (msg && !msg.read) {
///     const parsed = isPermissionResponse(msg.text)
///     if (parsed && parsed.request_id === request.id) { ... }
///   }
/// }
/// ```
///
/// Index-ordered and `!msg.read`-gated exactly like CC, because the read mark is
/// taken by index (`markMessageAsReadByIndex`) and an already-read row must not
/// be re-processed. `subtype === 'success'` → approved with the leader's
/// `response.updated_input` / `response.permission_updates`; anything else →
/// rejected with `parsed.error` as the feedback (CC `:408-421`).
fn scan_teammate_mailbox_for_permission_response(
    identity: &TeammateContext,
    request_id: &str,
) -> bool {
    let all_messages = crate::utils::teammate_mailbox::read_mailbox(
        &identity.agent_name,
        Some(&identity.team_name),
    );
    for (index, message) in all_messages.iter().enumerate() {
        if message.read {
            continue;
        }
        let Some(parsed) = crate::utils::teammate_mailbox::is_permission_response(&message.text)
        else {
            continue;
        };
        if parsed.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id) {
            continue;
        }
        crate::utils::teammate_mailbox::mark_message_as_read_by_index(
            &identity.agent_name,
            Some(&identity.team_name),
            index,
        );
        let approved = parsed.get("subtype").and_then(serde_json::Value::as_str) == Some("success");
        let response = parsed.get("response");
        crate::hooks::use_swarm_permission_poller::process_mailbox_permission_response(
            request_id,
            if approved { "approved" } else { "rejected" },
            parsed.get("error").and_then(serde_json::Value::as_str),
            response
                .and_then(|response| response.get("updated_input"))
                .cloned()
                .filter(|value| !value.is_null()),
            response
                .and_then(|response| response.get("permission_updates"))
                .filter(|value| !value.is_null()),
        );
        return true;
    }
    false
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#sendMessageToLeader`.
fn send_message_to_leader(from: &str, text: String, color: Option<&str>, team_name: &str) {
    if let Err(error) = crate::utils::teammate_mailbox::write_to_mailbox(
        crate::utils::swarm::constants::TEAM_LEAD_NAME,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: from.to_string(),
            text,
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: color.map(ToOwned::to_owned),
            summary: None,
        },
        Some(team_name),
    ) {
        tracing::warn!(%error, %from, %team_name, "failed to deliver in-process teammate message");
    }
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#sendIdleNotification`.
fn send_idle_notification(
    agent_name: &str,
    agent_color: Option<&str>,
    team_name: &str,
    idle_reason: Option<&str>,
    summary: Option<&str>,
) {
    let notification = crate::utils::teammate_mailbox::create_idle_notification(
        agent_name,
        idle_reason,
        summary,
        None,
        None,
        None,
    );
    send_message_to_leader(agent_name, notification.to_string(), agent_color, team_name);
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts` per-message body inside
/// `for await (const message of runAgent(...))`: append the live message,
/// update progress counters, and maintain in-progress tool-use IDs.
pub(crate) fn mirror_teammate_live_message(
    task_id: &str,
    message: Message,
    progress_tracker: &mut crate::tasks::local_agent_task::ProgressTracker,
) -> bool {
    crate::tasks::local_agent_task::update_progress_from_message(progress_tracker, &message);
    let progress = crate::tasks::local_agent_task::get_progress_update(progress_tracker);
    let appended = append_teammate_message(task_id, message.clone());
    let updated = update_teammate_progress_from_message(task_id, &message, progress);
    appended || updated
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#findAvailableTask`.
pub(crate) fn find_available_task(
    tasks: &[crate::utils::tasks::TaskRecord],
) -> Option<crate::utils::tasks::TaskRecord> {
    let unresolved_task_ids = tasks
        .iter()
        .filter(|task| task.status != "completed")
        .map(|task| task.id.clone())
        .collect::<std::collections::HashSet<_>>();

    tasks
        .iter()
        .find(|task| {
            task.status == "pending"
                && task.owner.is_none()
                && task
                    .blocked_by
                    .iter()
                    .all(|id| !unresolved_task_ids.contains(id))
        })
        .cloned()
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#formatTaskAsPrompt`.
pub(crate) fn format_task_as_prompt(task: &crate::utils::tasks::TaskRecord) -> String {
    let mut prompt = format!(
        "Complete all open tasks. Start with task #{}: \n\n {}",
        task.id, task.subject
    );
    if !task.description.trim().is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&task.description);
    }
    prompt
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#tryClaimNextTask`.
pub(crate) fn try_claim_next_task(task_list_id: &str, agent_name: &str) -> Option<String> {
    let tasks = crate::utils::tasks::list_tasks(task_list_id);
    let available_task = find_available_task(&tasks)?;
    let result = crate::utils::tasks::claim_task(task_list_id, &available_task.id, agent_name);
    if !result.success {
        tracing::debug!(
            task_id = %available_task.id,
            reason = ?result.reason,
            "in-process teammate failed to claim task"
        );
        return None;
    }
    let _ = crate::utils::tasks::update_task(
        task_list_id,
        &available_task.id,
        crate::utils::tasks::TaskUpdatePatch {
            status: Some("in_progress".to_string()),
            ..Default::default()
        },
    );
    Some(format_task_as_prompt(&available_task))
}

async fn wait_for_next_prompt_or_shutdown_with_interval(
    identity: &TeammateIdentity,
    abort_controller: &AbortController,
    task_id: &str,
    poll_interval: Duration,
) -> WaitResult {
    let mut poll_count = 0usize;
    while !abort_controller.is_aborted() {
        if let Some(message) = pop_pending_user_message_from_teammate(task_id) {
            return WaitResult::NewMessage {
                message,
                from: "user".to_string(),
                color: None,
                summary: None,
            };
        }

        if poll_count > 0 {
            tokio::time::sleep(poll_interval).await;
        }
        poll_count += 1;

        if abort_controller.is_aborted() {
            return WaitResult::Aborted;
        }

        let all_messages = crate::utils::teammate_mailbox::read_mailbox(
            &identity.agent_name,
            Some(&identity.team_name),
        );

        // Maps to CC prioritizing shutdown requests over earlier unread DMs.
        if let Some((idx, message, request)) = all_messages
            .iter()
            .enumerate()
            .filter(|(_, message)| !message.read)
            .find_map(|(idx, message)| {
                crate::utils::teammate_mailbox::is_shutdown_request(&message.text)
                    .map(|request| (idx, message, request))
            })
        {
            crate::utils::teammate_mailbox::mark_message_as_read_by_index(
                &identity.agent_name,
                Some(&identity.team_name),
                idx,
            );
            return WaitResult::ShutdownRequest {
                request,
                original_message: message.text.clone(),
            };
        }

        // Maps to CC prioritizing team-lead unread messages, then FIFO peers.
        let selected_index = all_messages
            .iter()
            .position(|message| {
                !message.read && message.from == crate::utils::swarm::constants::TEAM_LEAD_NAME
            })
            .or_else(|| all_messages.iter().position(|message| !message.read));

        if let Some(idx) = selected_index {
            if let Some(message) = all_messages.get(idx) {
                crate::utils::teammate_mailbox::mark_message_as_read_by_index(
                    &identity.agent_name,
                    Some(&identity.team_name),
                    idx,
                );
                return WaitResult::NewMessage {
                    message: message.text.clone(),
                    from: message.from.clone(),
                    color: message.color.clone(),
                    summary: message.summary.clone(),
                };
            }
        }

        if let Some(task_prompt) =
            try_claim_next_task(&identity.parent_session_id, &identity.agent_name)
        {
            return WaitResult::NewMessage {
                message: task_prompt,
                from: "task-list".to_string(),
                color: None,
                summary: None,
            };
        }
    }

    WaitResult::Aborted
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#waitForNextPromptOrShutdown`.
pub async fn wait_for_next_prompt_or_shutdown(
    identity: &TeammateIdentity,
    abort_controller: &AbortController,
    task_id: &str,
) -> WaitResult {
    wait_for_next_prompt_or_shutdown_with_interval(
        identity,
        abort_controller,
        task_id,
        Duration::from_millis(MAILBOX_POLL_INTERVAL_MS),
    )
    .await
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#startInProcessTeammate`.
pub fn start_in_process_teammate(config: InProcessRunnerConfig) -> Result<(), String> {
    // CC `inProcessRunner.ts:1549` `void runInProcessTeammate(config)` —
    // detached on Node's process event loop, "which can be hours for a
    // long-running teammate" (`:1547`). The spawning tool call runs inside a
    // query actor's private per-query runtime (`query.rs#spawn_query`), which
    // is dropped as soon as that turn resolves, so `Handle::try_current()`
    // would cap the teammate's life at the turn that created it.
    let handle = crate::utils::process_runtime::runtime_handle_for_detached_work().ok_or_else(|| {
        "In-process teammate execution requires an active async runtime; no teammate runner was started."
            .to_string()
    })?;
    handle.spawn(async move {
        if let Err(error) = run_in_process_teammate(config).await {
            tracing::warn!(error = %error, "in-process teammate runner failed");
        }
    });
    Ok(())
}

fn teammate_compaction_model(tool_use_context: &ToolUseContext) -> String {
    tool_use_context.main_loop_model.clone().unwrap_or_else(|| {
        crate::utils::model::model::get_main_loop_model()
    })
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` `resolvedAgentDefinition.tools`:
/// explicit custom-agent tools plus team-essential coordination tools, or `*`
/// when no custom tool list exists.
fn teammate_allowed_tools(custom_agent: Option<&AgentDefinition>) -> Vec<String> {
    let Some(custom_tools) = custom_agent.and_then(|agent| agent.tools.as_ref()) else {
        return vec!["*".to_string()];
    };
    let mut tools = custom_tools.clone();
    for required in [
        crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME,
        crate::tools::team_create_tool::prompt::TEAM_CREATE_TOOL_NAME,
        crate::tools::team_delete_tool::prompt::TEAM_DELETE_TOOL_NAME,
        crate::tools::task_create_tool::prompt::TASK_CREATE_TOOL_NAME,
        crate::tools::task_get_tool::prompt::TASK_GET_TOOL_NAME,
        crate::tools::task_list_tool::prompt::TASK_LIST_TOOL_NAME,
        crate::tools::task_update_tool::prompt::TASK_UPDATE_TOOL_NAME,
    ] {
        if !tools.iter().any(|tool| tool == required) {
            tools.push(required.to_string());
        }
    }
    tools
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` teammate prompt construction:
/// `getSystemPrompt(...) + TEAMMATE_SYSTEM_PROMPT_ADDENDUM` and optional
/// `# Custom Agent Instructions` from `agentDefinition.getSystemPrompt()`.
fn teammate_system_prompt(
    identity: &TeammateIdentity,
    tool_use_context: &ToolUseContext,
    model: Option<&str>,
    custom_agent: Option<&AgentDefinition>,
) -> String {
    let model = model
        .map(ToOwned::to_owned)
        .or_else(|| tool_use_context.main_loop_model.clone())
        .unwrap_or_else(|| {
            crate::utils::model::model::get_main_loop_model()
        });
    // Maps to CC `inProcessRunner.ts:928-933` `getSystemPrompt(
    // toolUseContext.options.tools, toolUseContext.options.mainLoopModel,
    // undefined, toolUseContext.options.mcpClients)` — teammates pass NO
    // additional working directories.
    let mut parts = crate::constants::prompts::get_system_prompt(
        &tool_use_context.tools,
        &model,
        &[],
        &tool_use_context.mcp_state.clients,
    );
    parts.push(TEAMMATE_SYSTEM_PROMPT_ADDENDUM.to_string());
    if let Some(custom_prompt) = custom_agent
        .and_then(|agent| agent.system_prompt.as_deref())
        .filter(|prompt| !prompt.trim().is_empty())
    {
        parts.push(format!("\n# Custom Agent Instructions\n{custom_prompt}"));
    }
    if parts.is_empty() {
        return format!("In-process teammate: {}", identity.agent_name);
    }
    parts.join("\n")
}

/// Maps to CC `utils/swarm/inProcessRunner.ts` `resolvedAgentDefinition` plus
/// per-iteration permission-mode override from the teammate task state.
fn teammate_iteration_agent_definition(
    identity: &TeammateIdentity,
    task_id: &str,
    tool_use_context: &ToolUseContext,
    model: Option<&str>,
    custom_agent: Option<&AgentDefinition>,
) -> AgentDefinition {
    let mut definition = AgentDefinition::new(
        identity.agent_name.clone(),
        format!("In-process teammate: {}", identity.agent_name),
        crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
    );
    definition.system_prompt = Some(teammate_system_prompt(
        identity,
        tool_use_context,
        model,
        custom_agent,
    ));
    definition.tools = Some(teammate_allowed_tools(custom_agent));
    definition.model = custom_agent.and_then(|agent| agent.model.clone());
    definition.permission_mode = Some(
        crate::tasks::in_process_teammate_task::get_in_process_teammate_task(task_id)
            .map(|task| task.permission_mode)
            .unwrap_or(PermissionMode::Default),
    );
    definition
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:1071-1126` — the compaction
/// block before `forkContextMessages` construction in
/// `runInProcessTeammate(...)`.
///
/// ```ts
/// let contextMessages = allMessages
/// const tokenCount = tokenCountWithEstimation(allMessages)
/// if (tokenCount > getAutoCompactThreshold(toolUseContext.options.mainLoopModel)) {
/// ```
///
/// The token budget is the WHOLE gate: CC has no message-count floor here. A
/// `|| all_messages.len() < 3` short-circuit used to sit next to the threshold
/// on this side with no citation behind it — unreachable in practice (three
/// messages cannot outweigh an auto-compact threshold) but the exact shape a
/// later reader mistakes for a CC branch. Removed rather than annotated.
async fn maybe_compact_teammate_history(
    identity: &TeammateIdentity,
    task_id: &str,
    all_messages: &mut Vec<Message>,
    current_user_message: &Message,
    tool_use_context: &mut ToolUseContext,
    teammate_replacement_state: &mut Option<
        crate::utils::tool_result_storage::ContentReplacementState,
    >,
) -> anyhow::Result<()> {
    let token_count = crate::utils::tokens::token_count_with_estimation(all_messages);
    let model = teammate_compaction_model(tool_use_context);
    let threshold = crate::services::compact::auto_compact::get_auto_compact_threshold(&model);
    if token_count <= threshold {
        return Ok(());
    }

    tracing::debug!(
        agent_id = %identity.agent_id,
        token_count,
        threshold,
        "compacting in-process teammate history"
    );

    // Maps to CC's isolated ToolUseContext copy: clone the context and clear
    // UI progress callbacks so teammate compaction cannot mutate the leader's
    // interactive progress state.
    let mut isolated_context = tool_use_context.clone();
    isolated_context.tool_progress_sink = crate::tool::ToolProgressSink::default();
    let compacted = crate::services::compact::compact::compact_conversation(
        all_messages.clone(),
        &isolated_context,
        &crate::services::compact::auto_compact::AutoCompactCacheSafeParams::default(),
        true,
        None,
        true,
    )
    .await
    .map_err(|error| anyhow::anyhow!(error))?;
    // `compact_conversation` rebuilt the Arc-shared authoritative cache in
    // place. Its returned Vec is reporting/persistence data, not a completion
    // delta, and restore Reads already inserted their source-position triggers.
    tool_use_context.loaded_nested_memory_paths.clear();
    let context_messages =
        crate::services::compact::compact::build_post_compact_messages(&compacted);

    // Maps to CC `runPostCompactCleanup(agent:*)` after a full compact replaces
    // previous message/tool ids while preserving main-thread context caches.
    crate::services::compact::post_compact_cleanup::run_post_compact_cleanup(Some(
        &crate::constants::query_source::QuerySource::Agent,
    ));
    if teammate_replacement_state.is_some() {
        *teammate_replacement_state =
            Some(crate::utils::tool_result_storage::ContentReplacementState::new());
    }

    all_messages.clear();
    all_messages.extend(context_messages.clone());

    let mut task_messages = context_messages;
    task_messages.push(current_user_message.clone());
    replace_teammate_messages(task_id, task_messages);
    Ok(())
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:1128-1131`.
///
/// ```ts
/// // Pass previous messages as context to preserve conversation history
/// // allMessages accumulates all previous messages (user + assistant) from prior iterations
/// const forkContextMessages =
///   contextMessages.length > 0 ? [...contextMessages] : undefined
/// ```
///
/// `contextMessages` is `allMessages` itself (`:1072`), or the post-compact
/// replacement that was just written back into it (`:1104-1116`); either way its
/// contents equal the accumulator's at this point, so the accumulator is the
/// argument here.
///
/// The `.length > 0` guard is what makes a teammate's FIRST turn behave like any
/// other fresh subagent: an empty accumulator yields `undefined`, and
/// `runAgent.ts:375-378` reads that carrier with `!== undefined` to pick
/// `createFileStateCacheWithSizeLimit(...)` over
/// `cloneFileStateCache(toolUseContext.readFileState)`. So the arm flip between
/// turn 1 and turn 2+ is CC's, not an accident of this port: turn 1 gets a fresh
/// Read ledger, turn 2+ inherits a clone of the LEADER's (the runner's context
/// is the leader's, and CC re-clones it every turn rather than carrying the
/// teammate's own ledger forward).
///
/// The snapshot is a copy (`[...contextMessages]`), not the live array, because
/// the accumulator keeps growing during the turn (`:1222`) while `runAgent`
/// holds this list as the prefix of `initialMessages`.
///
/// No cap: CC bounds this accumulator ONLY through the auto-compaction block
/// above ([`maybe_compact_teammate_history`]). `TEAMMATE_MESSAGES_UI_CAP` /
/// `appendCappedMessage` bound `task.messages`, the AppState UI mirror, and
/// never touch what the model sees.
fn teammate_fork_context_messages(context_messages: &[Message]) -> Option<Vec<Message>> {
    if context_messages.is_empty() {
        return None;
    }
    Some(context_messages.to_vec())
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:1197`.
///
/// ```ts
/// override: { abortController: currentWorkAbortController },
/// ```
///
/// The WHOLE literal. The controller carrier is why Escape stops this turn only
/// while the lifecycle controller it descends from still kills the whole
/// teammate; without it `runAgent.ts:524-528` would park an async agent under an
/// UNLINKED controller that ignores both.
///
/// **No `agentId`.** Census —
/// `ast-grep run --lang ts --pattern '({ override: $A })' --selector pair`,
/// then the same query on `--lang tsx` — finds every `override:` key CC hands
/// `runAgent`, ten of them:
///
/// - two are the shared base params, `systemPrompt`-or-`undefined` ternaries
///   the call sites spread from (AgentTool.tsx:899, resumeAgent.ts:183);
/// - six name an agent: AgentTool.tsx:1007/:1134/:1242 and resumeAgent.ts:238
///   (`{ ...runAgentParams.override, agentId, abortController }`),
///   SkillTool.ts:235 and processSlashCommand.tsx:230 (`{ agentId }`);
/// - two do not: magicDocs.ts:203, a fire-and-forget one-shot with no agent
///   identity to pin, and this one.
///
/// So the teammate runner is the only `runAgent` caller in CC that HOLDS a
/// stable agent identity and deliberately declines to pin it, leaving
/// `runAgent.ts:347` `override?.agentId ? override.agentId : createAgentId()`
/// to mint a FRESH id every teammate turn.
///
/// That asymmetry is the point: for the six that pin, one `runAgent` call is
/// one agent lifetime and the caller must key task state on the id, while here
/// one call is one TURN of a long-lived teammate. The teammate's lifetime
/// identity has its own carriers — `AgentContext.agentId` (`:909-921`),
/// `teamContext.teammates` (`spawnInProcess.ts:251`), the mailbox `workerId`
/// (`:344`), the SDK bookend `summary` (`:1459`) — and none of them is the run
/// id.
///
/// Everything keyed on the run id is register/release balanced INSIDE the run:
/// frontmatter session hooks (`runAgent.ts:568` register, `:821` clear, read
/// back as `toolUseContext.agentId ?? getSessionId()` where the child context
/// carries that same id — so per-turn ids re-register at the top of each turn
/// and clear in the same turn's `finally`, leaving nothing stale and nothing
/// leaked; between turns the registry simply holds no entry for the teammate),
/// todos (`:839-843`), shell tasks (`:847`), perfetto (`:358`/`:832`).
///
/// Pinning `identity.agent_id` here (an import-time invention, 519627e, whose
/// comment cited a `:1195-1198` member CC does not pass) fed a `formatAgentId`
/// `name@team` string into the `createAgentId` `a<hex>` namespace, and — because
/// every turn re-sends the whole accumulated history as `initialMessages` via
/// `fork_context_messages`, while neither CC's `insertMessageChain`
/// (`sessionStorage.ts:993-1048`) nor this port's dedupes — made turn N append
/// an Nth copy of that history to one `subagents/<id>.jsonl` under duplicate
/// uuids, which the leaf-to-root `parentUuid` walk that reads it back cannot
/// tell apart.
fn teammate_turn_override(
    current_work: &AbortController,
) -> crate::tools::agent_tool::run_agent::RunAgentOverride<'static> {
    crate::tools::agent_tool::run_agent::RunAgentOverride {
        abort_controller: Some(current_work.clone()),
        ..Default::default()
    }
}

/// What the prompt loop does after [`finish_teammate_turn`] has applied a
/// turn's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeammateTurnVerdict {
    /// CC `inProcessRunner.ts:1347` falls through to
    /// `waitForNextPromptOrShutdown` — the teammate is idle and alive, and the
    /// `while` will hand it the next prompt.
    Idle,
    /// CC `:1288` `break` out of the `while`, i.e. the lifecycle controller
    /// fired. The teammate is over.
    Ended,
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts:1279-1347` — the whole stretch
/// between `runAgent` finishing and `waitForNextPromptOrShutdown`.
///
/// CC consumes `runAgent` as an async generator and can therefore decide the
/// turn's fate from INSIDE the `for await`: `:1205-1210` breaks on the
/// lifecycle controller, `:1213-1219` breaks on the turn controller and sets
/// `workWasAborted`, and only an exception escaping the generator reaches the
/// `:1465` catch that ends the teammate. Rust's `run_agent` is a single
/// awaited call, so the same three-way split is decided here, from the
/// returned value plus the two controllers.
///
/// Extracted from the loop body (as [`maybe_compact_teammate_history`] and
/// [`teammate_fork_context_messages`] already are) because the decision is
/// what the port got wrong and a test has to be able to reach it without an
/// API round trip.
fn finish_teammate_turn(
    identity: &TeammateIdentity,
    task_id: &str,
    abort_controller: &AbortController,
    current_work: &AbortController,
    result: anyhow::Result<crate::tools::agent_tool::run_agent::RunAgentOutcome>,
    all_messages: &mut Vec<Message>,
    teammate_replacement_state: &mut Option<
        crate::utils::tool_result_storage::ContentReplacementState,
    >,
) -> anyhow::Result<TeammateTurnVerdict> {
    // Maps to: CC `:1279-1289` — clear the turn controller ("it's no longer
    // valid"), then check the LIFECYCLE controller FIRST, because that one
    // kills the whole teammate.
    clear_teammate_current_work(task_id);
    if abort_controller.is_aborted() {
        fail_in_process_teammate_task(task_id, "In-process teammate execution aborted");
        return Ok(TeammateTurnVerdict::Ended);
    }

    // Maps to: CC `:1157` `let workWasAborted = false`, set at `:1217` by the
    // in-loop `if (currentWorkAbortController.signal.aborted)` check. The
    // producer is `useBackgroundTaskNavigation.ts:157-158` — "Abort
    // currentWorkAbortController (stops current turn) NOT abortController
    // (kills teammate)".
    //
    // Read AFTER the lifecycle check: `AbortController::is_aborted` walks
    // parents (`tool.rs:300-303`) and this controller is a child of the
    // lifecycle one, so surviving that check is what makes the flag mean "the
    // TURN was aborted" and nothing else — the same discrimination CC gets
    // from testing `abortController` at `:1287` before `workWasAborted` at
    // `:1292`.
    let work_was_aborted = current_work.is_aborted();

    match result {
        Ok(crate::tools::agent_tool::run_agent::RunAgentOutcome::Completed(completed)) => {
            // Live UI state was already mirrored by `on_agent_message` as each
            // query message arrived. Keep the future prompt context and
            // sustained replacement state in sync without duplicating task
            // scrollback rows.
            *teammate_replacement_state = completed.content_replacement_state.clone();
            all_messages.extend(completed.messages);
        }
        Ok(crate::tools::agent_tool::run_agent::RunAgentOutcome::Backgrounded(_)) => {
            fail_in_process_teammate_task(
                task_id,
                "In-process teammate unexpectedly requested foreground background transfer",
            );
            return Ok(TeammateTurnVerdict::Ended);
        }
        // Maps to: CC `:1212-1219` — the turn-abort `break` out of
        // `for await (const message of runAgent(...))`.
        //
        // CC discovers the abort in the CONSUMER and breaks, so the generator
        // is closed by `.return()`: its `finally` (`runAgent.ts:816-858`) runs
        // but the `if (agentAbortController.signal.aborted) throw new
        // AbortError()` at `:808-810` never does, and nothing reaches the outer
        // catch. Rust has no generator to break out of, so `run_agent` surfaces
        // that same state as `Err(AgentExecutionAborted)`
        // (`run_agent.rs:1000-1011`, produced exactly when
        // `context.abort_controller` — this turn's controller — is aborted).
        // Downcasting is therefore the port's form of CC's break/throw split:
        // this error is the break, every other error is the throw that lands in
        // the `:1465` catch and ends the teammate.
        //
        // `agent_messages` is what CC's `:1222` `allMessages.push(message)` had
        // already accumulated when the flag flipped; CC's `:1213` check runs
        // before the push, so the message that arrives WITH the abort is
        // dropped on both sides.
        Err(error) => {
            match error.downcast::<crate::tools::agent_tool::run_agent::AgentExecutionAborted>() {
                Ok(aborted) => {
                    // CC `:1293-1295` `logForDebugging(...work interrupted,
                    // returning to idle)`.
                    tracing::debug!(
                        agent_id = %identity.agent_id,
                        "in-process teammate work interrupted, returning to idle"
                    );
                    // CC keeps the aborted turn's replacement decisions for
                    // free: `:1043` `let teammateReplacementState` is a
                    // reference to the one object `enforceToolResultBudget`
                    // mutates in place ("MUTATED: seenIds and replacements are
                    // updated in place", `toolResultStorage.ts:759-762`), and
                    // the `:1213-1219` break does not touch it. Only the
                    // compaction reset at `:1111-1113` ever rebinds it. Same
                    // assignment as the `Completed` arm above, because this
                    // port's state travels by value on the return.
                    //
                    // Losing it was invisible until 948daa7 gave an aborted
                    // turn a NEXT turn: the state reverted to its pre-turn
                    // value, so turn N+1 re-decided over results turn N had
                    // already frozen or replaced, changing the wire prefix the
                    // `:1035-1042` comment exists to keep stable.
                    *teammate_replacement_state = aborted.content_replacement_state;
                    all_messages.extend(aborted.agent_messages);
                }
                Err(error) => {
                    fail_in_process_teammate_task(task_id, error.to_string());
                    send_idle_notification(
                        &identity.agent_name,
                        identity.color.as_deref(),
                        &identity.team_name,
                        Some("failed"),
                        None,
                    );
                    return Err(error);
                }
            }
        }
    }

    // Maps to: CC `:1291-1309`.
    //
    // ```ts
    // if (workWasAborted) {
    //   logForDebugging(`... work interrupted, returning to idle`)
    //   const interruptMessage = createAssistantAPIErrorMessage({
    //     content: ERROR_MESSAGE_USER_ABORT,
    //   })
    //   updateTaskState(taskId, task => ({ ...task,
    //     messages: appendCappedMessage(task.messages, interruptMessage),
    //   }), setAppState)
    // }
    // ```
    //
    // Into `task.messages` (the AppState scrollback) ONLY — never into
    // `allMessages`, so the next turn's model context does not carry an abort
    // notice. Then the same idle transition and the same
    // `waitForNextPromptOrShutdown` a normal turn takes: an interrupted turn is
    // a turn boundary, not the end of the teammate.
    if work_was_aborted {
        append_teammate_message(
            task_id,
            Message::Assistant(crate::utils::messages::create_assistant_api_error_message(
                crate::services::compact::compact::ERROR_MESSAGE_USER_ABORT.to_string(),
                None,
                None,
            )),
        );
    }

    // Maps to: CC `:1317-1326` — idle, NOT completed, and fire the waiters.
    mark_teammate_idle(task_id);
    let peer_summary = crate::utils::teammate_mailbox::get_last_peer_dm_summary(all_messages);
    send_idle_notification(
        &identity.agent_name,
        identity.color.as_deref(),
        &identity.team_name,
        // Maps to: CC `:1339` `idleReason: workWasAborted ? 'interrupted' :
        // 'available'`. The `summary` beside it is outside that ternary and is
        // sent on both branches.
        Some(if work_was_aborted {
            "interrupted"
        } else {
            "available"
        }),
        peer_summary.as_deref(),
    );
    Ok(TeammateTurnVerdict::Idle)
}

/// Maps to: CC `utils/swarm/inProcessRunner.ts#runInProcessTeammate` prompt
/// loop. This port keeps the teammate alive between turns and mirrors the
/// official task claiming, mailbox, permission, live-progress, and in-process
/// compaction branches. Permission asks bubble to the leader through the
/// per-turn [`teammate_leader_dialog_sink`] wrapper this installs (CC
/// `:1179-1192`), or [`resolve_teammate_ask_via_mailbox`] when no leader sink
/// is installed.
pub async fn run_in_process_teammate(config: InProcessRunnerConfig) -> anyhow::Result<()> {
    let InProcessRunnerConfig {
        identity,
        task_id,
        prompt,
        description,
        model,
        allowed_tools,
        agent_definition,
        teammate_context,
        mut tool_use_context,
        abort_controller,
        invoking_request_id: _,
    } = config;

    let mut current_prompt = format_as_teammate_message(
        crate::utils::swarm::constants::TEAM_LEAD_NAME,
        &prompt,
        None,
        description.as_deref(),
    );
    let mut should_append_prompt_to_task = true;
    let mut all_messages: Vec<Message> = Vec::new();
    // Maps to CC `utils/swarm/inProcessRunner.ts` `teammateReplacementState`:
    // in-process teammates reset from the parent gate once, then carry the same
    // state across prompt-loop iterations for stable replacement decisions.
    let mut teammate_replacement_state = tool_use_context
        .content_replacement_state
        .as_ref()
        .map(|_| crate::utils::tool_result_storage::ContentReplacementState::new());
    // Maps to CC `inProcessRunner.ts:104` `getLeaderToolUseConfirmQueue` — the
    // leader's dialog leg as this port carries it. Captured ONCE, before the
    // loop wraps it per turn, so iteration N+1 wraps the leader's sink and not
    // iteration N's wrapper (which closes over a dead turn controller).
    let leader_permission_sink = tool_use_context.interactive_permission_sink.clone();

    let _ = try_claim_next_task(&identity.parent_session_id, &identity.agent_name);

    while !abort_controller.is_aborted() {
        if should_append_prompt_to_task {
            append_teammate_message(&task_id, teammate_user_message(current_prompt.clone()));
        }

        let user_message = teammate_user_message(current_prompt.clone());
        // Maps to: CC `inProcessRunner.ts:1068-1069` — `const userMessage =
        // createUserMessage({ content: currentPrompt }); const promptMessages:
        // Message[] = [userMessage]`. The turn's prompt is THIS message alone;
        // everything earlier reaches the model through `forkContextMessages`
        // below, which is the leg `runAgent.ts:370-373` runs
        // `filterIncompleteToolCalls` over.
        let prompt_messages = vec![user_message.clone()];
        maybe_compact_teammate_history(
            &identity,
            &task_id,
            &mut all_messages,
            &user_message,
            &mut tool_use_context,
            &mut teammate_replacement_state,
        )
        .await?;
        // Maps to: CC `inProcessRunner.ts:1128-1135`, in this order — snapshot
        // FIRST, push after, so the turn's own user message is not sent twice
        // (once as context, once as the prompt).
        let fork_context_messages = teammate_fork_context_messages(&all_messages);
        all_messages.push(user_message);

        let current_work = AbortController::child_of(abort_controller.clone());
        mark_teammate_running(&task_id, Some(current_work.clone()));
        tool_use_context.abort_controller = current_work.clone();
        // The teammate's LIFETIME identity, on the context the RUNNER owns —
        // not the run id. CC keeps that identity in
        // `AgentContext { agentId: identity.agentId, ... }`
        // (`inProcessRunner.ts:909-921`), an AsyncLocalStorage scope this port
        // has no equivalent of, so the runner's own context is where it rides.
        // `create_subagent_context` overwrites the field for the CHILD with
        // this turn's `createAgentId()` (`run_agent.rs:878`, CC
        // `runAgent.ts:700-702`), which is what the turn's hooks, todos, shell
        // tasks and sidechain transcript key on — see the `override` below for
        // why those two ids are deliberately different.
        tool_use_context.agent_id = Some(identity.agent_id.clone());
        // No `tool_use_context.messages` reset here: CC strips the parent
        // conversation ONCE at the spawn site (`spawnMultiAgent.ts:927-931`,
        // "Strip messages: the teammate never reads toolUseContext.messages (it
        // builds its own history via allMessages in inProcessRunner). Passing
        // the parent's full conversation here would pin it for the teammate's
        // lifetime, surviving /clear and auto-compact"), which this port does
        // at `spawn_multi_agent.rs:559` `context.clone().with_messages(...)`,
        // and the runner loop never touches the field again. The per-iteration
        // re-clear that used to sit here was a no-op duplicate whose comment
        // misattributed the missing history to it.

        // Maps to: CC `inProcessRunner.ts:1179-1192` — `canUseTool:
        // createInProcessCanUseTool(identity, currentWorkAbortController,
        // waitMs => updateTaskState(taskId, ... totalPausedMs ...))`, rebuilt
        // per iteration around THIS turn's controller. The guard is CC's own
        // `if (setToolUseConfirmQueue)` (`:198`): with no leader queue the ask
        // must still reach the mailbox fallback, and
        // `run_agent.rs#ask_parent_for_agent_permission` selects that leg by
        // `interactive_permission_sink.is_some()` — installing a wrapper over an
        // absent leader sink would report a dialog leg that cannot answer.
        if leader_permission_sink.is_some() {
            tool_use_context.interactive_permission_sink = teammate_leader_dialog_sink(
                leader_permission_sink.clone(),
                task_id.clone(),
                tool_use_context.abort_controller.clone(),
            );
        }
        let progress_tracker = Arc::new(Mutex::new(
            crate::tasks::local_agent_task::create_progress_tracker(),
        ));
        let live_task_id = task_id.clone();
        let live_progress_tracker = Arc::clone(&progress_tracker);
        let on_agent_message = move |message: &Message| {
            if let Ok(mut tracker) = live_progress_tracker.lock() {
                mirror_teammate_live_message(&live_task_id, message.clone(), &mut tracker);
            }
        };

        // Maps to: CC inProcessRunner.ts:928 await getSystemPrompt(...).
        // A4: the sync prompt reader waits on plugin command promises; this
        // runner itself lives on the published process executor.
        let prompt_identity = identity.clone();
        let prompt_task_id = task_id.clone();
        let prompt_context = tool_use_context.clone();
        let prompt_model = model.clone();
        let prompt_agent = agent_definition.clone();
        let prompt_teammate_scope = crate::utils::teammate_context::capture_teammate_context();
        let iteration_agent_definition = tokio::task::spawn_blocking(move || {
            crate::utils::teammate_context::with_teammate_context_sync(
                prompt_teammate_scope,
                || {
                    teammate_iteration_agent_definition(
                        &prompt_identity,
                        &prompt_task_id,
                        &prompt_context,
                        prompt_model.as_deref(),
                        prompt_agent.as_ref(),
                    )
                },
            )
        })
        .await?;
        // Maps to CC `inProcessRunner.ts:1160-1203`: the iteration runs inside
        // `runWithTeammateContext(teammateContext, ...)` (ALS → Rust
        // task-local scope) so `isInProcessTeammate()` / `getParentSessionId()`
        // observe this teammate. In-process teammates are async but run in the
        // same process as the leader, so they CAN show permission prompts
        // (`allowPermissionPrompts ?? true` — the option is not threaded
        // through the Rust spawn path yet, so the `?? true` default applies),
        // preserve tool use results for their viewable transcripts, and
        // receive the LEADER's tool pool
        // (`availableTools: toolUseContext.options.tools`), not a recomputed
        // worker pool.
        let result = crate::utils::teammate_context::run_with_teammate_context(
            teammate_context.clone(),
            crate::tools::agent_tool::run_agent::run_agent(
                crate::tools::agent_tool::run_agent::RunAgentInput {
                    agent_definition: &iteration_agent_definition,
                    prompt: &current_prompt,
                    description: description.as_deref(),
                    model_override: model.as_deref(),
                    context: &tool_use_context,
                    query_source: crate::constants::query_source::QuerySource::AgentCustom,
                    is_async: true,
                    can_show_permission_prompts: Some(true),
                    available_tools: Some(tool_use_context.tools.clone()),
                    // Maps to: CC `inProcessRunner.ts:1195`
                    // `forkContextMessages,` — "Pass forkContextMessages to
                    // preserve conversation history across prompts" (`:1171`).
                    // Turn 1 sends `None` (empty accumulator), so
                    // `runAgent.ts:375-378` gives it a FRESH read-file cache;
                    // turn 2+ sends `Some`, which is the same predicate that
                    // flips that call to `cloneFileStateCache(
                    // toolUseContext.readFileState)`. See
                    // [`teammate_fork_context_messages`].
                    fork_context_messages,
                    preserve_tool_use_results: true,
                    transcript_subdir: None,
                    r#override: teammate_turn_override(&tool_use_context.abort_controller),
                    use_exact_tools: false,
                    allowed_tools: allowed_tools.as_deref(),
                    worktree_path: None,
                    // #156: no canUseTool param — CC `inProcessRunner.ts:1179`
                    // hands `createInProcessCanUseTool(...)` into runAgent so
                    // `query()` threads it to the child's tool execution; this
                    // port's teammate permission behaviors live on the ask
                    // path the child actually reaches
                    // (`run_agent.rs#ask_parent_for_agent_permission`: the
                    // `teammate_leader_dialog_sink` wrapper installed above,
                    // which `create_subagent_context`'s clone carries to the
                    // child, or `resolve_teammate_ask_via_mailbox` when no
                    // leader sink exists).
                    parent_tool_use_id: None,
                    on_progress: None,
                    background_task_id: None,
                    content_replacement_state: teammate_replacement_state.clone(),
                    background_signal: None,
                    on_message: Some(&on_agent_message),
                    // Maps to: CC `inProcessRunner.ts:1177` `promptMessages,`
                    // — CC's `promptMessages` is `[userMessage]` (`:1069`),
                    // never the accumulator. Handing the whole history here
                    // instead (the old shape) reached the model as
                    // `initialMessages` all the same, but bypassed BOTH
                    // `runAgent.ts:370-373`'s `filterIncompleteToolCalls` and
                    // the `:375-378` read-file-cache arm, which key off
                    // `forkContextMessages` alone.
                    prompt_messages: Some(prompt_messages),
                },
            ),
        )
        .await;

        match finish_teammate_turn(
            &identity,
            &task_id,
            &abort_controller,
            &current_work,
            result,
            &mut all_messages,
            &mut teammate_replacement_state,
        )? {
            // CC `:1288` `break`, or the port-only backgrounded case: the
            // teammate is over and the loop's terminal write already happened.
            TeammateTurnVerdict::Ended => return Ok(()),
            // CC falls through from `:1347` to `waitForNextPromptOrShutdown`.
            TeammateTurnVerdict::Idle => {}
        }

        match wait_for_next_prompt_or_shutdown(&identity, &abort_controller, &task_id).await {
            WaitResult::ShutdownRequest {
                request,
                original_message,
            } => {
                let from = request
                    .get("from")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(crate::utils::swarm::constants::TEAM_LEAD_NAME);
                current_prompt = format_as_teammate_message(from, &original_message, None, None);
                should_append_prompt_to_task = true;
            }
            WaitResult::NewMessage {
                message,
                from,
                color,
                summary,
            } => {
                if from == "user" {
                    current_prompt = message;
                    // `injectUserMessageToTeammate` already mirrors this row.
                    should_append_prompt_to_task = false;
                } else {
                    current_prompt = format_as_teammate_message(
                        &from,
                        &message,
                        color.as_deref(),
                        summary.as_deref(),
                    );
                    should_append_prompt_to_task = true;
                }
            }
            WaitResult::Aborted => break,
        }
    }

    if abort_controller.is_aborted() {
        fail_in_process_teammate_task(&task_id, "In-process teammate execution aborted");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn identity(name: &str) -> TeammateIdentity {
        TeammateIdentity {
            agent_id: format!("{name}@alpha"),
            agent_name: name.to_string(),
            team_name: "alpha".to_string(),
            color: Some("green".to_string()),
            plan_mode_required: false,
            parent_session_id: "session-parent".to_string(),
        }
    }

    fn register_task(name: &str) -> String {
        let task_id = format!("task-{name}-{}", uuid::Uuid::new_v4());
        crate::tasks::in_process_teammate_task::register_in_process_teammate_task(
            crate::tasks::in_process_teammate_task::RegisterInProcessTeammateParams {
                task_id: task_id.clone(),
                identity: identity(name),
                description: "runner test".to_string(),
                prompt: "help".to_string(),
                selected_agent: None,
                model: None,
                permission_mode: crate::types::permissions::PermissionMode::Default,
                tool_use_id: None,
            },
        );
        task_id
    }

    /// The ask→mailbox route resolves the leader via
    /// `permission_sync::get_leader_name`, which reads the team file from disk
    /// (CC `permissionSync.ts:657` `readTeamFileAsync`). A memory-only seed is
    /// therefore a no-op fixture; pin a scratch config root and enable the
    /// team-file and mailbox disk paths. The returned guards must outlive the
    /// test body.
    fn seed_team_for_permission_tests() -> (EnvVarGuard, EnvVarGuard, EnvVarGuard) {
        let root = std::env::temp_dir().join(format!(
            "cometix-in-process-runner-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        let record = crate::utils::swarm::team_helpers::create_team_record(
            "alpha".to_string(),
            None,
            Some(crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()),
            None,
            "/tmp".to_string(),
        );
        crate::utils::swarm::team_helpers::write_team_record(record);
        (config_guard, io_guard, write_guard)
    }

    fn teammate_context(name: &str) -> TeammateContext {
        crate::utils::teammate_context::create_teammate_context(
            crate::utils::teammate_context::CreateTeammateContextConfig {
                agent_id: format!("{name}@alpha"),
                agent_name: name.to_string(),
                team_name: "alpha".to_string(),
                color: Some("green".to_string()),
                plan_mode_required: false,
                parent_session_id: "session-parent".to_string(),
                abort_controller: AbortController::default(),
            },
        )
    }

    fn mailbox_ask_request(
        tool_use_id: &str,
        command: &str,
    ) -> crate::types::permissions::PermissionRequest {
        let input = serde_json::json!({ "command": command });
        crate::utils::permissions::permissions::mock_permission_request_with_input(
            format!("perm-{tool_use_id}"),
            tool_use_id.to_string(),
            "Bash".to_string(),
            command.to_string(),
            input,
            PermissionMode::Default,
        )
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    #[test]
    fn format_as_teammate_message_wraps_initial_prompt_like_official() {
        let text =
            format_as_teammate_message("team-lead", "please inspect", Some("blue"), Some("audit"));
        assert!(text.contains("<teammate-message teammate_id=\"team-lead\""));
        assert!(text.contains("color=\"blue\""));
        assert!(text.contains("summary=\"audit\""));
        assert!(text.contains("please inspect"));
        assert!(text.contains("</teammate-message>"));
    }

    #[test]
    fn teammate_iteration_agent_definition_matches_official_default_and_custom_shape() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let identity = identity("reviewer");
        let task_id = register_task("reviewer");
        let context = ToolUseContext::default().with_main_loop_model("claude-sonnet-4-6");

        let default_definition =
            teammate_iteration_agent_definition(&identity, &task_id, &context, None, None);
        assert_eq!(default_definition.agent_type, "reviewer");
        assert_eq!(
            default_definition.tools.as_deref(),
            Some(&["*".to_string()][..])
        );
        assert!(
            default_definition
                .system_prompt
                .as_deref()
                .unwrap_or_default()
                .contains("# Agent Teammate Communication")
        );
        assert_eq!(
            default_definition.permission_mode,
            Some(PermissionMode::Default)
        );

        let mut custom = AgentDefinition::new(
            "critic",
            "review changes",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
        );
        custom.system_prompt = Some("Use the project's review rubric.".to_string());
        custom.tools = Some(vec!["Bash".to_string()]);
        custom.model = Some("haiku".to_string());
        let custom_definition = teammate_iteration_agent_definition(
            &identity,
            &task_id,
            &context,
            Some("opus"),
            Some(&custom),
        );
        let tools = custom_definition.tools.unwrap();
        assert!(tools.contains(&"Bash".to_string()));
        assert!(tools.contains(
            &crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME.to_string()
        ));
        assert!(
            tools.contains(
                &crate::tools::team_delete_tool::prompt::TEAM_DELETE_TOOL_NAME.to_string()
            )
        );
        assert_eq!(custom_definition.model.as_deref(), Some("haiku"));
        assert!(
            custom_definition
                .system_prompt
                .as_deref()
                .unwrap_or_default()
                .contains("# Custom Agent Instructions\nUse the project's review rubric.")
        );
    }

    #[test]
    fn mirror_teammate_live_message_updates_task_during_agent_iteration() {
        let task_id = register_task("live-progress");
        let message = Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId("toolu_live".to_string()),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command":"cargo test"}),
                },
            )],
            model: None,
            stop_reason: None,
            usage: Some(crate::types::message::TokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                cache_creation_input_tokens: 1,
                cache_read_input_tokens: 2,
                cache_deleted_input_tokens: 0,
            }),
        });
        let mut tracker = crate::tasks::local_agent_task::create_progress_tracker();

        assert!(mirror_teammate_live_message(
            &task_id,
            message.clone(),
            &mut tracker,
        ));

        let state = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task registered");
        assert_eq!(state.messages, vec![message]);
        assert_eq!(state.progress.as_ref().unwrap().tool_use_count, 1);
        assert_eq!(state.progress.as_ref().unwrap().token_count, 17);
        assert!(state.in_progress_tool_use_ids.contains("toolu_live"));
    }

    #[tokio::test]
    async fn teammate_history_compaction_does_not_fake_success_when_summary_aborts() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1");

        let task_id = register_task("compact");
        let identity = identity("compact");
        let mut all_messages = (0..6)
            .map(|index| teammate_user_message(format!("older teammate turn {index}")))
            .collect::<Vec<_>>();
        let current_user_message = teammate_user_message("new compact-triggering turn".to_string());
        let mut replacement_state =
            Some(crate::utils::tool_result_storage::ContentReplacementState::new());
        replacement_state
            .as_mut()
            .unwrap()
            .seen_ids
            .insert("toolu_old".to_string());
        let mut context = ToolUseContext::default()
            .with_main_loop_model(crate::utils::model::model::get_main_loop_model());
        context.abort_controller.abort();
        let original_messages = all_messages.clone();

        let error = maybe_compact_teammate_history(
            &identity,
            &task_id,
            &mut all_messages,
            &current_user_message,
            &mut context,
            &mut replacement_state,
        )
        .await
        .expect_err("aborted compaction must not synthesize a local summary");

        assert!(
            error
                .to_string()
                .contains(crate::services::compact::compact::ERROR_MESSAGE_USER_ABORT)
        );
        assert_eq!(all_messages, original_messages);
        assert!(
            replacement_state
                .as_ref()
                .unwrap()
                .seen_ids
                .contains("toolu_old")
        );

        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    /// One aborted teammate turn, set up the way
    /// `useBackgroundTaskNavigation.ts:157-158` sets it up: the TURN controller
    /// is aborted ("stops current turn") while the lifecycle controller it
    /// descends from is untouched ("NOT abortController (kills teammate)").
    fn escaped_turn(
        task_id: &str,
        agent_messages: Vec<Message>,
    ) -> (
        AbortController,
        AbortController,
        anyhow::Result<crate::tools::agent_tool::run_agent::RunAgentOutcome>,
    ) {
        escaped_turn_with_replacement_state(task_id, agent_messages, None)
    }

    /// The same escaped turn, with the replacement state `run_agent` hands back
    /// out of `run_agent.rs:1010` — the port's carrier for the object CC's
    /// break leaves mutated in the runner's own scope.
    fn escaped_turn_with_replacement_state(
        task_id: &str,
        agent_messages: Vec<Message>,
        content_replacement_state: Option<
            crate::utils::tool_result_storage::ContentReplacementState,
        >,
    ) -> (
        AbortController,
        AbortController,
        anyhow::Result<crate::tools::agent_tool::run_agent::RunAgentOutcome>,
    ) {
        let lifecycle = AbortController::default();
        let current_work = AbortController::child_of(lifecycle.clone());
        mark_teammate_running(task_id, Some(current_work.clone()));
        current_work.abort();
        let result = Err(anyhow::Error::new(
            crate::tools::agent_tool::run_agent::AgentExecutionAborted {
                agent_messages,
                content_replacement_state,
            },
        ));
        (lifecycle, current_work, result)
    }

    /// Maps to: CC `inProcessRunner.ts:1287-1292`, `:1317-1326`, `:1354` — a
    /// turn-only abort survives the lifecycle check, sets `workWasAborted`,
    /// marks the task idle (`isIdle: true`, status stays `'running'`) and falls
    /// through to `waitForNextPromptOrShutdown`, so the `while` hands the
    /// teammate its next prompt.
    ///
    /// Old shape: `run_agent`'s `Err(AgentExecutionAborted)`
    /// (`run_agent.rs:1010`, raised exactly when the turn controller is
    /// aborted) hit an `Err(_)` arm that ran `fail_in_process_teammate_task` and
    /// `return`ed — one Escape left a terminal `failed` task, and
    /// `inject_user_message_to_teammate` refuses terminal tasks
    /// (`in_process_teammate_task.rs:540-542`), so the leader could no longer
    /// reach the teammate at all. That old shape FAILS this test on the status
    /// assert; nothing here awaits, so it cannot hang.
    #[test]
    fn escaped_teammate_turn_goes_idle_alive_and_still_takes_the_next_prompt() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();

        let identity = identity("reviewer");
        let task_id = register_task("reviewer");
        let (lifecycle, current_work, result) = escaped_turn(
            &task_id,
            vec![assistant_text("got halfway through the review")],
        );

        let mut all_messages = vec![teammate_user_message("review this".to_string())];
        let mut replacement_state = None;
        let verdict = finish_teammate_turn(
            &identity,
            &task_id,
            &lifecycle,
            &current_work,
            result,
            &mut all_messages,
            &mut replacement_state,
        )
        .expect("a turn-only abort is CC's `break`, not the `:1465` catch");

        assert_eq!(
            verdict,
            TeammateTurnVerdict::Idle,
            "CC falls through from `:1347` into `waitForNextPromptOrShutdown`"
        );

        let task = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task still registered");
        assert_eq!(
            task.status, "running",
            "CC `:1318-1326` writes only `isIdle`; the teammate is idle, not terminal"
        );
        assert!(task.is_idle);
        assert!(
            task.error.is_none(),
            "an interrupted turn is not a teammate failure"
        );
        assert!(
            task.current_work_abort_controller.is_none(),
            "CC `:1280-1284` clears the turn controller once it is no longer valid"
        );

        // CC `:1222` pushed each message as it arrived and `:1213` broke before
        // pushing the one that came WITH the abort, so what the turn produced
        // stays in the accumulator and reaches the next turn's context.
        assert_eq!(
            all_messages.iter().map(message_text).collect::<Vec<_>>(),
            vec![
                "review this".to_string(),
                "got halfway through the review".to_string(),
            ]
        );

        // The teammate is addressable again — the whole point of aborting the
        // turn instead of the teammate.
        assert!(
            crate::tasks::in_process_teammate_task::inject_user_message_to_teammate(
                &task_id,
                "ok, try the other file"
            ),
            "a live teammate accepts the leader's next prompt"
        );
        assert_eq!(
            crate::tasks::in_process_teammate_task::pop_pending_user_message_from_teammate(
                &task_id
            )
            .as_deref(),
            Some("ok, try the other file"),
            "and the prompt loop's `waitForNextPromptOrShutdown` can pop it"
        );
    }

    /// Maps to: CC `inProcessRunner.ts:1296-1308` and `:1339`.
    ///
    /// ```ts
    /// const interruptMessage = createAssistantAPIErrorMessage({
    ///   content: ERROR_MESSAGE_USER_ABORT,
    /// })
    /// ...
    /// idleReason: workWasAborted ? 'interrupted' : 'available',
    /// ```
    ///
    /// The row goes to `task.messages` (the scrollback) ONLY, never to
    /// `allMessages` — so the next turn's model context does not inherit an
    /// abort notice.
    ///
    /// Old shape: neither existed. The scrollback ended on the last real message
    /// and the leader got `idleReason: "failed"` with the abort's message as the
    /// failure reason. Fails on the message assert.
    #[test]
    fn escaped_teammate_turn_lands_cc_user_abort_row_and_an_interrupted_idle_reason() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();

        let identity = identity("reviewer");
        let task_id = register_task("reviewer");
        let (lifecycle, current_work, result) = escaped_turn(&task_id, Vec::new());

        let mut all_messages = vec![teammate_user_message("review this".to_string())];
        let mut replacement_state = None;
        finish_teammate_turn(
            &identity,
            &task_id,
            &lifecycle,
            &current_work,
            result,
            &mut all_messages,
            &mut replacement_state,
        )
        .expect("a turn-only abort is CC's `break`");

        let task = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task still registered");
        let last = task.messages.last().expect("the interrupt row is appended");
        assert_eq!(
            message_text(last),
            crate::services::compact::compact::ERROR_MESSAGE_USER_ABORT,
            "CC `ERROR_MESSAGE_USER_ABORT` (compact.ts:295), not the Rust error's Display"
        );
        assert!(
            matches!(last, Message::Assistant(_)),
            "CC `createAssistantAPIErrorMessage`, so the scrollback renders it as \
             the interrupted-turn row"
        );
        assert!(
            !all_messages.iter().any(|message| message_text(message)
                == crate::services::compact::compact::ERROR_MESSAGE_USER_ABORT),
            "CC `:1301-1308` touches `task.messages` only — the model context \
             must not inherit the abort notice"
        );

        let inbox = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        assert_eq!(inbox.len(), 1, "CC `:1334-1342` still notifies the leader");
        let parsed = crate::utils::teammate_mailbox::is_idle_notification(&inbox[0].text)
            .expect("an idle notification, not a failure");
        assert_eq!(
            parsed.get("idleReason").and_then(serde_json::Value::as_str),
            Some("interrupted")
        );
    }

    /// Maps to: CC `inProcessRunner.ts:1035-1045` + `:1202` +
    /// `toolResultStorage.ts:759-762`.
    ///
    /// The runner's `teammateReplacementState` is ONE object, created once
    /// before the `while` and handed to every turn's `runAgent`;
    /// `enforceToolResultBudget` mutates it in place ("MUTATED: seenIds and
    /// replacements are updated in place to record choices made this call. The
    /// caller holds a stable reference across turns"). The `:1213-1219` turn
    /// break does not rebind it — only the compaction reset at `:1111-1113`
    /// ever does — so an aborted CC turn KEEPS the decisions it made, and turn
    /// N+1 re-applies the cached preview for `frozen`/`mustReapply` ids
    /// (`:642-667` `partitionByPriorDecision`) instead of re-deciding
    /// holistically. That is the whole point of `:1035-1042`: "Without
    /// persisting state across iterations ... wire prefix differs → cache
    /// miss."
    ///
    /// Old shape: `AgentExecutionAborted` carried only `agent_messages`, so
    /// `finish_teammate_turn`'s abort arm left `teammate_replacement_state` at
    /// its PRE-turn value — the aborted turn's decisions were dropped. Fails
    /// this test on both `seen_ids`/`replacements` asserts (it observes the
    /// empty pre-turn state); nothing here awaits, so it cannot hang.
    ///
    /// It was unreachable before 948daa7 (#194) because the old turn shape
    /// killed the teammate on Escape, so there was no turn N+1 to lose it for.
    #[test]
    fn escaped_teammate_turn_keeps_its_replacement_decisions_so_the_next_turn_reapplies_not_redecides()
     {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();

        let identity = identity("reviewer");
        let task_id = register_task("reviewer");

        // What `run_agent` hands back at `run_agent.rs:1010`: the state its
        // `QueryEvent::ContentReplacementStateUpdate` arm rebound during the
        // turn that was then aborted. `toolu_frozen` was seen and left alone,
        // `toolu_replaced` was persisted and previewed.
        let mut turn_state = crate::utils::tool_result_storage::ContentReplacementState::new();
        turn_state.seen_ids.insert("toolu_frozen".to_string());
        turn_state.seen_ids.insert("toolu_replaced".to_string());
        turn_state.replacements.insert(
            "toolu_replaced".to_string(),
            "[large tool result preview]".to_string(),
        );
        let (lifecycle, current_work, result) = escaped_turn_with_replacement_state(
            &task_id,
            vec![assistant_text("read three large files")],
            Some(turn_state),
        );

        // The runner's pre-turn value: gated on the parent (`:1043-1045`), so
        // `Some`, but empty — this is what the old shape left behind.
        let mut all_messages = vec![teammate_user_message("review this".to_string())];
        let mut replacement_state =
            Some(crate::utils::tool_result_storage::ContentReplacementState::new());
        finish_teammate_turn(
            &identity,
            &task_id,
            &lifecycle,
            &current_work,
            result,
            &mut all_messages,
            &mut replacement_state,
        )
        .expect("a turn-only abort is CC's `break`");

        let carried = replacement_state.expect("the parent gate is still on after the abort");
        assert!(
            carried.seen_ids.contains("toolu_frozen"),
            "CC `:658-659`: a result seen and left unreplaced is frozen forever. \
             Dropping it lets turn N+1 replace it, changing an already-cached prefix"
        );
        assert_eq!(
            carried
                .replacements
                .get("toolu_replaced")
                .map(String::as_str),
            Some("[large tool result preview]"),
            "CC `:655-657` re-applies the cached preview byte-identically every \
             turn. Dropping it sends the FULL original content next turn — the \
             prefix the aborted turn already cached no longer matches"
        );
    }

    /// The fence on the other side of the same split: CC only swallows the turn
    /// abort. An exception that escapes the `runAgent` generator still reaches
    /// the `inProcessRunner.ts:1465` catch, which marks the task `failed`, sends
    /// `idleReason: 'failed'` (`:1516-1525`) and returns.
    #[test]
    fn a_teammate_turn_that_fails_for_any_other_reason_still_ends_the_teammate() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();

        let identity = identity("reviewer");
        let task_id = register_task("reviewer");
        let lifecycle = AbortController::default();
        let current_work = AbortController::child_of(lifecycle.clone());
        mark_teammate_running(&task_id, Some(current_work.clone()));

        let mut all_messages = Vec::new();
        let mut replacement_state = None;
        let error = finish_teammate_turn(
            &identity,
            &task_id,
            &lifecycle,
            &current_work,
            Err(anyhow::anyhow!("upstream connection reset")),
            &mut all_messages,
            &mut replacement_state,
        )
        .expect_err("a non-abort error is CC's throw, not its break");
        assert!(error.to_string().contains("upstream connection reset"));

        let task = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task still registered");
        assert_eq!(task.status, "failed");
        assert_eq!(task.error.as_deref(), Some("upstream connection reset"));
        let inbox = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        let parsed = crate::utils::teammate_mailbox::is_idle_notification(&inbox[0].text).unwrap();
        assert_eq!(
            parsed.get("idleReason").and_then(serde_json::Value::as_str),
            Some("failed")
        );
    }

    /// Maps to: CC `inProcessRunner.ts:1197` `override: { abortController:
    /// currentWorkAbortController },` — the only `runAgent` caller in CC that
    /// holds a stable agent identity and does not pin it (census in
    /// [`teammate_turn_override`]), so `runAgent.ts:347` `override?.agentId ?
    /// override.agentId : createAgentId()` mints a fresh id every teammate
    /// turn.
    ///
    /// Old shape: `agent_id: Some(&identity.agent_id)`, pinned across every turn
    /// under a comment citing a `:1195-1198` member CC does not pass. The
    /// assertion below is why that was not merely redundant — a
    /// `formatAgentId` identity is not a `createAgentId` id, and everything
    /// downstream of `runAgent`'s `agentId` (the `subagents/<id>.jsonl`
    /// transcript, `<id>.meta.json`, the frontmatter-hook registry key, the
    /// todos key, shell-task ownership) is in the minted namespace. Fails on
    /// `agent_id.is_none()`.
    #[test]
    fn teammate_turn_override_carries_the_controller_and_never_pins_an_agent_id() {
        let lifecycle = AbortController::default();
        let current_work = AbortController::child_of(lifecycle.clone());
        let r#override = teammate_turn_override(&current_work);

        assert!(
            r#override.agent_id.is_none(),
            "CC `:1197` passes abortController alone"
        );
        assert!(
            r#override
                .abort_controller
                .as_ref()
                .expect("CC `:1197` DOES pass the turn controller")
                .same_identity(&current_work),
            "`runAgent.ts:524-528` picks this over the unlinked async controller"
        );

        // CC `utils/uuid.ts#createAgentId` mints `a<16 hex>`; the teammate's
        // lifetime identity comes from `formatAgentId` and is `<name>@<team>`.
        // Two namespaces, and only the first one belongs in this field.
        let minted = crate::tools::agent_tool::run_agent::create_agent_id(None);
        let is_minted_shape = |id: &str| {
            id.len() == 17
                && id.starts_with('a')
                && id[1..].chars().all(|byte| byte.is_ascii_hexdigit())
        };
        assert!(is_minted_shape(&minted));
        assert!(
            !is_minted_shape(&identity("reviewer").agent_id),
            "the id the override used to pin was never a run agent id"
        );
        assert_ne!(
            minted,
            crate::tools::agent_tool::run_agent::create_agent_id(None),
            "consecutive turns therefore get distinct run ids"
        );
    }

    /// Maps to: CC `inProcessRunner.ts:198-208`, `:250-291` — the standard leg
    /// hands the ask to the leader's queue and reports the wait it cost into
    /// `onPermissionWaitMs` (`:262` on allow) → `totalPausedMs` (`:1182-1191`).
    #[test]
    fn teammate_dialog_ask_reports_its_wait_into_total_paused_ms() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let task_id = register_task("reviewer");

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let leader_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                leader.setter.clone(),
            );
        let abort = AbortController::default();
        let sink = teammate_leader_dialog_sink(leader_sink, task_id.clone(), abort.clone());

        // The leader takes a measurable moment before answering; CC measures
        // exactly that span (`Date.now() - permissionStartMs`).
        let versions = leader.versions.clone();
        let answerer = std::thread::spawn(move || {
            let queue = versions.recv_blocking().expect("entry must be queued");
            std::thread::sleep(Duration::from_millis(20));
            assert!(
                queue[0].responder.respond(
                    crate::types::permissions::PermissionPromptResponse::new(
                        crate::types::permissions::PermissionPromptChoice::AllowOnce,
                    )
                ),
                "the teammate must still be waiting on the entry"
            );
            queue[0].clone()
        });

        let response = block_on(
            sink.ask(
                crate::tool::InteractivePermissionAsk::new(mailbox_ask_request(
                    "toolu_dialog",
                    "cargo build",
                ))
                .with_worker_badge(Some(
                    crate::types::permissions::PermissionWorkerBadge {
                        name: "reviewer".to_string(),
                        color: Some("green".to_string()),
                    },
                )),
            ),
        )
        .expect("the leader's answer resolves the teammate's ask");
        let queued = answerer.join().expect("answerer panicked");

        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::AllowOnce
        );
        // CC `:234-236` `workerBadge: identity.color ? {...} : undefined` — the
        // wrapper forwards it untouched.
        assert_eq!(
            queued
                .worker_badge
                .as_ref()
                .map(|badge| badge.name.as_str()),
            Some("reviewer")
        );
        let paused = crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
            .expect("task registered")
            .total_paused_ms
            .expect("the resolved ask reports its wait");
        assert!(
            paused >= 20,
            "reported wait must cover the time the dialog was up, got {paused}ms"
        );
    }

    /// Maps to: CC `inProcessRunner.ts:209-221` — the `'abort'` listener resolves
    /// the pending ask with `SUBAGENT_REJECT_MESSAGE` (this port: `None`, which
    /// the caller turns into the same deny), reports the wait
    /// (`:212 reportPermissionWait()`), and WITHDRAWS its own row from the
    /// leader's queue (`:214-216`), instead of leaving the teammate parked on a
    /// dialog only the leader can answer — or leaving the human answering a
    /// dialog nobody is parked on.
    ///
    /// OLD SHAPE: the withdrawal had no carrier
    /// (`leaderPermissionBridge.ts#getLeaderToolUseConfirmQueue` was missing),
    /// so the row survived until the REPL's own cancel path cleared the whole
    /// queue and answering it was a silent no-op. `leader.queue` would still
    /// hold one row at the end of this test.
    #[test]
    fn teammate_dialog_ask_withdraws_its_row_and_stops_waiting_when_the_turn_aborts() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _bridge_lock =
            crate::utils::swarm::leader_permission_bridge::TEST_LEADER_PERMISSION_BRIDGE_LOCK
                .lock()
                .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let task_id = register_task("reviewer");

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        // CC `:195` reads the setter off the bridge the REPL registered
        // (`REPL.tsx:1646`); the withdrawal goes back through the same slot.
        crate::utils::swarm::leader_permission_bridge::register_leader_tool_use_confirm_queue(
            leader.setter.clone(),
        );
        let leader_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                leader.setter.clone(),
            );
        let abort = AbortController::default();
        let sink = teammate_leader_dialog_sink(leader_sink, task_id.clone(), abort.clone());

        // Nobody ever answers; the turn is aborted while the dialog is up.
        let versions = leader.versions.clone();
        let aborter = {
            let abort = abort.clone();
            std::thread::spawn(move || {
                let queue = versions.recv_blocking().expect("entry must be queued");
                assert_eq!(queue.len(), 1);
                abort.abort();
                // Hold the entry (and its responder) alive so the ask can only
                // end through the abort race, not through a dropped channel.
                std::thread::sleep(Duration::from_millis(50));
                drop(queue);
            })
        };

        let response = block_on(sink.ask(crate::tool::InteractivePermissionAsk::new(
            mailbox_ask_request("toolu_dialog_abort", "cargo build"),
        )));
        aborter.join().expect("aborter panicked");

        assert!(response.is_none(), "abort must not wait for the leader");
        assert!(
            leader.queue.lock().unwrap().is_empty(),
            "CC `:214-216` withdraws the row the aborted ask owned"
        );
        assert!(
            crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
                .expect("task registered")
                .total_paused_ms
                .is_some(),
            "CC reports the wait on the abort path too (`:212`)"
        );

        crate::utils::swarm::leader_permission_bridge::unregister_leader_tool_use_confirm_queue();
    }

    /// The withdrawal's null branch: CC's `:214-216` runs
    /// `setToolUseConfirmQueue(...)` on the setter captured at `:195`, which is
    /// only reached inside `if (setToolUseConfirmQueue)`. With no registered
    /// leader queue there is nothing to withdraw from, and the abort must still
    /// stop the teammate waiting.
    #[test]
    fn teammate_dialog_abort_without_a_registered_bridge_still_stops_waiting() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        let _bridge_lock =
            crate::utils::swarm::leader_permission_bridge::TEST_LEADER_PERMISSION_BRIDGE_LOCK
                .lock()
                .unwrap();
        crate::utils::swarm::leader_permission_bridge::unregister_leader_tool_use_confirm_queue();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let task_id = register_task("reviewer");

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let leader_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                leader.setter.clone(),
            );
        let abort = AbortController::default();
        let sink = teammate_leader_dialog_sink(leader_sink, task_id.clone(), abort.clone());

        let versions = leader.versions.clone();
        let aborter = std::thread::spawn(move || {
            let queue = versions.recv_blocking().expect("entry must be queued");
            abort.abort();
            std::thread::sleep(Duration::from_millis(50));
            drop(queue);
        });

        let response = block_on(sink.ask(crate::tool::InteractivePermissionAsk::new(
            mailbox_ask_request("toolu_dialog_abort_no_bridge", "cargo build"),
        )));
        aborter.join().expect("aborter panicked");

        assert!(response.is_none());
        assert_eq!(leader.queue.lock().unwrap().len(), 1);
    }

    /// Maps to: CC `inProcessRunner.ts:179-181` / `:191-193` — the two
    /// pre-dialog `abortController.signal.aborted` checks: nothing is queued,
    /// and `reportPermissionWait` is never reached.
    #[test]
    fn teammate_dialog_ask_queues_nothing_when_already_aborted() {
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        let task_id = register_task("reviewer");

        let leader = crate::utils::swarm::leader_permission_bridge::test_leader_queue();
        let leader_sink =
            crate::hooks::tool_permission::handlers::interactive_handler::create_repl_interactive_permission_sink(
                leader.setter.clone(),
            );
        let abort = AbortController::default();
        abort.abort();
        let sink = teammate_leader_dialog_sink(leader_sink, task_id.clone(), abort);

        let response = block_on(sink.ask(crate::tool::InteractivePermissionAsk::new(
            mailbox_ask_request("toolu_dialog_pre_abort", "cargo build"),
        )));

        assert!(response.is_none());
        assert!(
            leader.queue.lock().unwrap().is_empty(),
            "no dialog may be queued"
        );
        assert!(
            crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
                .expect("task registered")
                .total_paused_ms
                .is_none(),
            "a prompt that was never shown costs no paused time"
        );
    }

    /// Maps to: CC `inProcessRunner.ts:337-447` — the mailbox fallback forwards
    /// the ask to the leader, and the ONLY non-answer exit is abort
    /// (`:388-392` interval body, `:435-442` listener), which yields `None` so
    /// the caller keeps its deny. This test used to pin a
    /// `COMETIX_SWARM_PERMISSION_WAIT_MS` timeout; CC has no deadline on either
    /// this leg or `swarmWorkerHandler.ts:67-147`, so the bound is gone and the
    /// abort exit is what is pinned. #156: the former
    /// `create_in_process_can_use_tool` variant of this test also pinned a
    /// `pendingWorkerRequest` app-state write, which is deliberately GONE —
    /// CC's `createInProcessCanUseTool` never writes it (zero hits in
    /// `inProcessRunner.ts`; that state belongs to `swarmWorkerHandler.ts`).
    #[test]
    fn teammate_ask_mailbox_forwards_request_and_stays_unresolved_on_abort() {
        let _permission_lock = crate::utils::swarm::permission_sync::TEST_PERMISSION_SYNC_LOCK
            .lock()
            .unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::permission_sync::clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _team_guards = seed_team_for_permission_tests();

        let identity = teammate_context("reviewer");
        let abort = AbortController::default();
        let request = mailbox_ask_request("toolu_bash", "rm -rf target");
        // CC registers the callback, sends, and only THEN reaches the abort
        // check, so an already-aborted turn still forwards the request.
        abort.abort();

        let response = block_on(resolve_teammate_ask_via_mailbox(
            &identity, &request, &abort,
        ));

        assert!(response.is_none(), "abort must stay unresolved");
        let pending = crate::utils::swarm::permission_sync::read_pending_permissions(Some("alpha"));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, "Bash");
        assert!(
            !crate::hooks::use_swarm_permission_poller::has_permission_callback(&pending[0].id),
            "CC `cleanup()` unregisters the response callback on the abort exit"
        );
        let leader_messages = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        assert_eq!(leader_messages.len(), 1);
        assert!(
            crate::utils::teammate_mailbox::is_permission_request(&leader_messages[0].text)
                .is_some()
        );
    }

    /// Maps to: CC `inProcessRunner.ts:394-425` + `:353-371` — the leg's real
    /// wake source: the leader writes a `permission_response` into the
    /// teammate's OWN mailbox (`permission_sync::send_permission_response_via_mailbox`,
    /// the exact call `repl.rs#send_mailbox_permission_prompt_response` makes,
    /// CC `useInboxPoller.ts:382-389`), the poll scans for it, marks it read,
    /// and `processMailboxPermissionResponse` resolves with `updated_input` and
    /// `permission_updates`.
    ///
    /// The previous version of this test drove `permission_sync::resolve_permission`
    /// instead — a producer with ZERO callers in either tree (CC:
    /// `permissionSync.ts:360` is defined and never called), so it proved the
    /// resolved-store read path and nothing the leader actually does.
    #[test]
    fn teammate_ask_mailbox_accepts_leader_permission_response() {
        let _permission_lock = crate::utils::swarm::permission_sync::TEST_PERMISSION_SYNC_LOCK
            .lock()
            .unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::permission_sync::clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _team_guards = seed_team_for_permission_tests();

        let resolver = std::thread::spawn(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let pending =
                    crate::utils::swarm::permission_sync::read_pending_permissions(Some("alpha"));
                if let Some(request) = pending.first() {
                    assert!(
                        crate::utils::swarm::permission_sync::send_permission_response_via_mailbox(
                            "reviewer",
                            &crate::utils::swarm::permission_sync::PermissionResolution {
                                decision: "approved".to_string(),
                                resolved_by: crate::utils::swarm::constants::TEAM_LEAD_NAME
                                    .to_string(),
                                feedback: None,
                                updated_input: Some(
                                    serde_json::json!({"command":"cargo test --quiet"})
                                ),
                                permission_updates: Some(vec![serde_json::json!({
                                    "type": "addRules",
                                    "destination": "session",
                                    "behavior": "allow",
                                    "rules": [{"toolName": "Bash", "ruleContent": "cargo test --quiet"}]
                                })]),
                            },
                            &request.id,
                            Some("alpha"),
                        )
                    );
                    return;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let identity = teammate_context("reviewer");
        let abort = AbortController::default();
        let request = mailbox_ask_request("toolu_bash_allow", "cargo test");

        let response = block_on(resolve_teammate_ask_via_mailbox_with_interval(
            &identity,
            &request,
            &abort,
            Duration::from_millis(10),
        ))
        .expect("leader approval resolves the ask");
        resolver.join().unwrap();

        assert_eq!(
            response.choice,
            crate::types::permissions::PermissionPromptChoice::AllowOnce
        );
        assert_eq!(
            response.updated_input.as_ref().unwrap()["command"].as_str(),
            Some("cargo test --quiet")
        );
        assert!(response.permission_updates.iter().any(|update| matches!(
            update,
            crate::types::permissions::PermissionUpdate::AddRules { rules, .. }
                if rules.iter().any(|rule| rule.rule_content.as_deref() == Some("cargo test --quiet"))
        )));
        // CC `:403` `markMessageAsReadByIndex(...)` before processing, so the
        // next poll tick cannot re-deliver the same answer.
        let teammate_inbox =
            crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        assert_eq!(teammate_inbox.len(), 1);
        assert!(teammate_inbox[0].read);
        // CC `processMailboxPermissionResponse` (`useSwarmPermissionPoller.ts:145`)
        // deletes the entry from the registry before invoking it.
        let pending = crate::utils::swarm::permission_sync::read_pending_permissions(Some("alpha"));
        assert_eq!(pending.len(), 1);
        assert!(
            !crate::hooks::use_swarm_permission_poller::has_permission_callback(&pending[0].id)
        );
    }

    #[test]
    fn send_idle_notification_writes_structured_message_to_leader_mailbox() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        send_idle_notification(
            "reviewer",
            Some("green"),
            "alpha",
            Some("available"),
            Some("[to tester] logs"),
        );
        let inbox = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "reviewer");
        assert_eq!(inbox[0].color.as_deref(), Some("green"));
        let parsed = crate::utils::teammate_mailbox::is_idle_notification(&inbox[0].text).unwrap();
        assert_eq!(
            parsed.get("from").and_then(serde_json::Value::as_str),
            Some("reviewer")
        );
        assert_eq!(
            parsed.get("idleReason").and_then(serde_json::Value::as_str),
            Some("available")
        );
    }

    #[test]
    fn task_prompt_helpers_match_official_availability_and_text() {
        let tasks = vec![
            crate::utils::tasks::TaskRecord {
                id: "1".to_string(),
                subject: "Blocking setup".to_string(),
                description: String::new(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
            crate::utils::tasks::TaskRecord {
                id: "2".to_string(),
                subject: "Implement feature".to_string(),
                description: "Use the official path".to_string(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: vec!["1".to_string()],
                metadata: None,
            },
        ];
        assert_eq!(find_available_task(&tasks).unwrap().id, "1");

        let mut completed_first = tasks.clone();
        completed_first[0].status = "completed".to_string();
        let available = find_available_task(&completed_first).unwrap();
        assert_eq!(available.id, "2");
        assert_eq!(
            format_task_as_prompt(&available),
            "Complete all open tasks. Start with task #2: \n\n Implement feature\n\nUse the official path"
        );
    }

    #[tokio::test]
    async fn wait_for_next_prompt_claims_available_task_list_work() {
        let _task_store_lock = crate::utils::tasks::TASK_TOOL_TEST_LOCK.lock().unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _task_lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        // Official waitForNextPromptOrShutdown receives the leader's
        // parentSessionId as taskListId; seed that exact list.
        let _temp = crate::utils::tasks::TempTaskConfig::new("session-parent");
        crate::utils::tasks::TASK_TOOL_STORE.lock().unwrap().push(
            crate::utils::tasks::TaskRecord {
                id: "7".to_string(),
                subject: "Review diff".to_string(),
                description: "Check edge cases".to_string(),
                active_form: None,
                status: "pending".to_string(),
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: None,
            },
        );
        let task_id = register_task("reviewer");

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_next_prompt_or_shutdown_with_interval(
                &identity("reviewer"),
                &AbortController::default(),
                &task_id,
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("seeded parent-session task should be claimed without polling forever");

        assert!(matches!(
            result,
            WaitResult::NewMessage { ref message, ref from, .. }
                if from == "task-list" && message.contains("task #7")
        ));
        let tasks = crate::utils::tasks::list_tasks(&crate::utils::tasks::get_task_list_id());
        assert_eq!(tasks[0].owner.as_deref(), Some("reviewer"));
        assert_eq!(tasks[0].status, "in_progress");
    }

    #[tokio::test]
    async fn wait_for_next_prompt_prioritizes_pending_user_message() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let task_id = register_task("reviewer");
        crate::tasks::in_process_teammate_task::inject_user_message_to_teammate(
            &task_id,
            "user wakeup",
        );
        crate::utils::teammate_mailbox::write_to_mailbox(
            "reviewer",
            crate::utils::teammate_mailbox::TeammateMessageInput {
                from: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
                text: "mailbox wakeup".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                color: None,
                summary: None,
            },
            Some("alpha"),
        )
        .unwrap();

        let result = wait_for_next_prompt_or_shutdown_with_interval(
            &identity("reviewer"),
            &AbortController::default(),
            &task_id,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            WaitResult::NewMessage { ref message, ref from, .. }
                if message == "user wakeup" && from == "user"
        ));
        assert!(
            crate::tasks::in_process_teammate_task::get_in_process_teammate_task(&task_id)
                .unwrap()
                .pending_user_messages
                .is_empty()
        );
    }

    #[tokio::test]
    async fn wait_for_next_prompt_prioritizes_shutdown_then_leader_messages() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _lock = crate::tasks::in_process_teammate_task::TEST_IN_PROCESS_TEAMMATE_TASK_LOCK
            .lock()
            .unwrap();
        crate::tasks::in_process_teammate_task::clear_in_process_teammate_tasks_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let task_id = register_task("reviewer");
        let reviewer = identity("reviewer");

        crate::utils::teammate_mailbox::write_to_mailbox(
            "reviewer",
            crate::utils::teammate_mailbox::TeammateMessageInput {
                from: "peer".to_string(),
                text: "peer chatter".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                color: Some("red".to_string()),
                summary: Some("chatter".to_string()),
            },
            Some("alpha"),
        )
        .unwrap();
        let shutdown = crate::utils::teammate_mailbox::create_shutdown_request_message(
            "shutdown-1",
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("done"),
        );
        crate::utils::teammate_mailbox::write_to_mailbox(
            "reviewer",
            crate::utils::teammate_mailbox::TeammateMessageInput {
                from: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
                text: shutdown.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                color: None,
                summary: None,
            },
            Some("alpha"),
        )
        .unwrap();

        let result = wait_for_next_prompt_or_shutdown_with_interval(
            &reviewer,
            &AbortController::default(),
            &task_id,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            WaitResult::ShutdownRequest { ref request, .. }
                if request.get("requestId").and_then(serde_json::Value::as_str) == Some("shutdown-1")
        ));
        let inbox = crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        assert!(!inbox[0].read, "peer message should remain unread");
        assert!(inbox[1].read, "shutdown request should be marked read");

        crate::utils::teammate_mailbox::write_to_mailbox(
            "reviewer",
            crate::utils::teammate_mailbox::TeammateMessageInput {
                from: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
                text: "leader follow-up".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                color: None,
                summary: None,
            },
            Some("alpha"),
        )
        .unwrap();
        let result = wait_for_next_prompt_or_shutdown_with_interval(
            &reviewer,
            &AbortController::default(),
            &task_id,
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            WaitResult::NewMessage { ref message, ref from, .. }
                if message == "leader follow-up" && from == crate::utils::swarm::constants::TEAM_LEAD_NAME
        ));
    }

    fn assistant_text(text: &str) -> Message {
        Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::Text(
                text.to_string(),
            )],
            model: None,
            stop_reason: None,
            usage: None,
        })
    }

    fn assistant_tool_use(id: &str) -> Message {
        Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::AssistantContent::ToolUse(
                crate::types::message::ToolUseBlock {
                    id: crate::types::ids::ToolUseId(id.to_string()),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command":"ls"}),
                },
            )],
            model: None,
            stop_reason: Some(crate::types::message::StopReason::ToolUse),
            usage: None,
        })
    }

    fn tool_result_message(id: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(crate::types::message::ToolResult {
                tool_use_id: crate::types::ids::ToolUseId(id.to_string()),
                content: "ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
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
        })
    }

    fn message_text(message: &Message) -> String {
        match message {
            Message::User(user) => user
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContent::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            Message::Assistant(assistant) => assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::types::message::AssistantContent::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
            _ => String::new(),
        }
    }

    /// One prompt-loop iteration's accumulator bookkeeping, exactly as
    /// `run_in_process_teammate` sequences it: snapshot (CC
    /// `inProcessRunner.ts:1130-1131`), push the turn's user message (`:1135`),
    /// then absorb everything `runAgent` yielded (`:1222`). Returns the pair the
    /// loop hands to `runAgent` — `(forkContextMessages, promptMessages)`.
    fn run_one_teammate_turn(
        all_messages: &mut Vec<Message>,
        prompt: &str,
        agent_output: Vec<Message>,
    ) -> (Option<Vec<Message>>, Vec<Message>) {
        let user_message = teammate_user_message(prompt.to_string());
        let prompt_messages = vec![user_message.clone()];
        let fork_context_messages = teammate_fork_context_messages(all_messages);
        all_messages.push(user_message);
        all_messages.extend(agent_output);
        (fork_context_messages, prompt_messages)
    }

    /// CC `inProcessRunner.ts:1128-1135` + `:1222` — "Pass previous messages as
    /// context to preserve conversation history … allMessages accumulates all
    /// previous messages (user + assistant) from prior iterations".
    ///
    /// The accumulator outlives the loop body (`:1004`, declared before the
    /// `while`), so a teammate's turn N sees turns 1..N-1: its own prompts, its
    /// own assistant output, and the tool_use/tool_result rows in between —
    /// `allMessages.push(message)` at `:1222` is unfiltered over everything
    /// `runAgent` yields.
    ///
    /// Old shape: the call site pinned `fork_context_messages: None`
    /// unconditionally, so `is_some()` on turn 2 FAILED (assert, not hang).
    /// The accumulator itself was already being kept, but it was routed through
    /// `prompt_messages`, which is a different carrier with different downstream
    /// behaviour — see the two tests below.
    #[test]
    fn teammate_turn_two_sees_turn_one_history_through_the_fork_carrier() {
        let mut all_messages: Vec<Message> = Vec::new();

        let (turn_one_context, turn_one_prompt) = run_one_teammate_turn(
            &mut all_messages,
            "<teammate-message>first task</teammate-message>",
            vec![assistant_text("done with the first task")],
        );
        assert!(
            turn_one_context.is_none(),
            "CC `contextMessages.length > 0 ? ... : undefined` — the very first \
             teammate turn has an empty accumulator"
        );
        assert_eq!(turn_one_prompt.len(), 1);

        let (turn_two_context, turn_two_prompt) = run_one_teammate_turn(
            &mut all_messages,
            "<teammate-message>what did you just do?</teammate-message>",
            vec![assistant_text("I finished the first task")],
        );

        let carried = turn_two_context.expect("turn 2 carries turn 1's conversation");
        assert_eq!(
            carried.iter().map(message_text).collect::<Vec<_>>(),
            vec![
                "<teammate-message>first task</teammate-message>".to_string(),
                "done with the first task".to_string(),
            ],
            "turn 2 is handed turn 1's user prompt AND turn 1's assistant reply"
        );
        // The snapshot is taken BEFORE the push (CC `:1130` then `:1135`), so
        // the turn's own prompt is not also part of its context prefix.
        assert!(
            !carried
                .iter()
                .any(|message| message_text(message).contains("what did you just do?")),
            "the current prompt travels in promptMessages only, never twice"
        );
        assert_eq!(
            turn_two_prompt.iter().map(message_text).collect::<Vec<_>>(),
            vec!["<teammate-message>what did you just do?</teammate-message>".to_string()],
            "CC `promptMessages: Message[] = [userMessage]` (`:1069`)"
        );

        // What `runAgent.ts:370-373` composes out of the pair — the model sees
        // history first, then this turn's prompt.
        let mut initial_messages =
            crate::tools::agent_tool::run_agent::filter_incomplete_tool_calls(&carried);
        initial_messages.extend(turn_two_prompt);
        assert_eq!(
            initial_messages
                .iter()
                .map(message_text)
                .collect::<Vec<_>>(),
            vec![
                "<teammate-message>first task</teammate-message>".to_string(),
                "done with the first task".to_string(),
                "<teammate-message>what did you just do?</teammate-message>".to_string(),
            ]
        );
    }

    /// CC `runAgent.ts:375-378` — `forkContextMessages !== undefined` picks
    /// `cloneFileStateCache(toolUseContext.readFileState)` over a fresh
    /// size-limited cache. The teammate loop therefore flips arms between its
    /// own turn 1 and turn 2, and that flip is CC's: `:1130`'s `.length > 0`
    /// guard is the only thing deciding it.
    ///
    /// The two arms themselves are pinned in `run_agent.rs`
    /// (`non_fork_subagent_starts_with_an_empty_read_file_state_so_edit_demands_its_own_read`
    /// and `fork_child_inherits_a_clone_of_the_parent_read_file_state`); what is
    /// pinned HERE is the discriminant this file produces for them.
    ///
    /// Old shape: `fork_context_messages: None` on every turn, so a teammate sat
    /// on the fresh arm forever — the assertion on turn 2 FAILED (assert, not
    /// hang). Note the inherited ledger is a clone of the LEADER's cache, not of
    /// the teammate's previous turn: CC re-clones `toolUseContext.readFileState`
    /// each turn and never carries the teammate's own forward.
    #[test]
    fn teammate_fork_carrier_flips_the_read_file_cache_arm_after_the_first_turn() {
        let mut all_messages: Vec<Message> = Vec::new();

        let (turn_one_context, _) =
            run_one_teammate_turn(&mut all_messages, "first", vec![assistant_text("ok")]);
        assert!(
            turn_one_context.is_none(),
            "turn 1 takes `createFileStateCacheWithSizeLimit(...)` — a teammate \
             must Read a file itself before Edit will touch it"
        );

        let (turn_two_context, _) =
            run_one_teammate_turn(&mut all_messages, "second", vec![assistant_text("ok")]);
        assert!(
            turn_two_context.is_some(),
            "turn 2+ takes `cloneFileStateCache(toolUseContext.readFileState)`"
        );

        let (turn_three_context, _) =
            run_one_teammate_turn(&mut all_messages, "third", vec![assistant_text("ok")]);
        assert!(
            turn_three_context.is_some(),
            "the arm does not flip back once the accumulator is non-empty"
        );
    }

    /// CC `runAgent.ts:368-373` — "Filter out incomplete tool calls from parent
    /// messages to avoid API errors". Routing the teammate's history through
    /// `forkContextMessages` is what subjects it to that filter.
    ///
    /// Old shape: the history rode in through `prompt_messages`, which
    /// `initial_agent_messages` returns untouched (`run_agent.rs:572`), so a
    /// tool_use left unanswered by an earlier turn was replayed to the API
    /// unpaired. This test FAILS on the old routing (the orphan survives), it
    /// does not hang.
    #[test]
    fn teammate_history_routed_through_the_fork_carrier_drops_unanswered_tool_uses() {
        let mut all_messages: Vec<Message> = Vec::new();
        run_one_teammate_turn(
            &mut all_messages,
            "run the build",
            vec![
                assistant_tool_use("toolu_answered"),
                tool_result_message("toolu_answered"),
                assistant_text("build is green"),
                // Yielded, never answered — the turn ended first.
                assistant_tool_use("toolu_orphan"),
            ],
        );

        let (carried, prompt_messages) =
            run_one_teammate_turn(&mut all_messages, "and now the tests", Vec::new());
        let carried = carried.expect("turn 2 carries turn 1");
        assert_eq!(
            carried.len(),
            5,
            "the carrier is the raw accumulator; filtering happens in runAgent"
        );

        let mut initial_messages =
            crate::tools::agent_tool::run_agent::filter_incomplete_tool_calls(&carried);
        initial_messages.extend(prompt_messages);
        assert_eq!(
            initial_messages.len(),
            5,
            "4 of the 5 carried rows survive the filter, plus this turn's prompt"
        );
        assert!(
            !initial_messages.iter().any(|message| matches!(
                message,
                Message::Assistant(assistant)
                    if assistant.content.iter().any(|block| matches!(
                        block,
                        crate::types::message::AssistantContent::ToolUse(tool_use)
                            if tool_use.id.0 == "toolu_orphan"
                    ))
            )),
            "the unanswered tool_use is dropped before the model sees it"
        );
        assert!(
            initial_messages.iter().any(|message| matches!(
                message,
                Message::Assistant(assistant)
                    if assistant.content.iter().any(|block| matches!(
                        block,
                        crate::types::message::AssistantContent::ToolUse(tool_use)
                            if tool_use.id.0 == "toolu_answered"
                    ))
            )),
            "the answered pair survives"
        );
    }

    /// CC bounds the teammate's model-facing history ONLY through the
    /// auto-compaction block at `inProcessRunner.ts:1071-1126`; there is no
    /// slice and no cap on `allMessages` itself.
    /// `TEAMMATE_MESSAGES_UI_CAP` / `appendCappedMessage`
    /// (`tasks/InProcessTeammateTask/types.ts:101-121`) bound `task.messages` —
    /// the AppState UI mirror — and the fork carrier must not inherit that cap,
    /// or a teammate would silently forget its own middle turns while the
    /// transcript still showed them.
    ///
    /// Old shape: the carrier was always `None`, so there was no list to cap or
    /// not cap; this asserts the bound chosen now, not a regression.
    #[test]
    fn teammate_fork_carrier_is_bounded_by_compaction_only_not_the_ui_cap() {
        let cap = crate::tasks::in_process_teammate_task::TEAMMATE_MESSAGES_UI_CAP;
        let mut all_messages: Vec<Message> = Vec::new();
        for turn in 0..cap {
            run_one_teammate_turn(
                &mut all_messages,
                &format!("turn {turn}"),
                vec![assistant_text(&format!("reply {turn}"))],
            );
        }

        let (carried, _) = run_one_teammate_turn(&mut all_messages, "final", Vec::new());
        let carried = carried.expect("the accumulator is long past empty");
        assert_eq!(
            carried.len(),
            cap * 2,
            "every prior user+assistant row is still carried"
        );
        assert!(
            carried.len() > cap,
            "the model-facing carrier is not the UI-capped list"
        );
        assert_eq!(message_text(&carried[0]), "turn 0");

        // The UI mirror, by contrast, is capped.
        let mut ui_messages: Vec<Message> = Vec::new();
        for message in &carried {
            ui_messages = crate::tasks::in_process_teammate_task::append_capped_message(
                &ui_messages,
                message.clone(),
            );
        }
        assert_eq!(ui_messages.len(), cap);
    }
}
