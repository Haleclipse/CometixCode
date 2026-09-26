//! Official teammate inbox polling helper.
//!
//! Maps to: CC `hooks/useInboxPoller.ts`.
//!
//! The TypeScript implementation is a React hook with three responsibilities:
//! polling unread mailbox messages, routing structured swarm protocol messages,
//! and delivering/queuing regular teammate messages.  This Rust slice keeps the
//! same boundary in `hooks/` and exposes a pure `poll_inbox_once`/`deliver_*`
//! core so the iocraft REPL can wire it without moving mailbox/runtime logic
//! into UI renderers.

use crate::constants::xml::TEAMMATE_MESSAGE_TAG;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    MailboxPermissionResponseTarget, PermissionBehavior, PermissionMode, PermissionRuleValue,
    PermissionUpdate, PermissionUpdateDestination, PermissionWorkerBadge, ToolUseConfirm,
};
use crate::utils::permissions::permission_mode::{
    external_permission_mode_from_string, permission_mode_from_string, to_external_permission_mode,
};
use crate::utils::permissions::permission_update::apply_permission_update;
use crate::utils::swarm::constants::TEAM_LEAD_NAME;
use crate::utils::teammate_mailbox::{
    TeammateMessage, TeammateMessageInput, create_plan_approval_response_message,
    is_mode_set_request, is_permission_request, is_permission_response, is_plan_approval_request,
    is_plan_approval_response, is_sandbox_permission_request, is_sandbox_permission_response,
    is_shutdown_approved, is_shutdown_request, is_team_permission_update, mark_messages_as_read,
    read_unread_messages, write_to_mailbox,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Maps to: CC `hooks/useInboxPoller.ts#INBOX_POLL_INTERVAL_MS`.
pub const INBOX_POLL_INTERVAL_MS: u64 = 1000;

/// Maps to: CC `AppState.teamContext.teammates[teammateId]` fields consumed by
/// `useInboxPoller.ts`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPollerTeammateInfo {
    pub name: String,
    pub agent_type: Option<String>,
    pub color: Option<String>,
    pub tmux_session_name: Option<String>,
    pub tmux_pane_id: Option<String>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub spawned_at: Option<u64>,
    pub backend_type: Option<String>,
}

/// Maps to: CC `AppState.teamContext` consumed by `useInboxPoller.ts`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPollerTeamContext {
    pub team_name: String,
    pub team_file_path: String,
    pub lead_agent_id: String,
    pub self_agent_id: Option<String>,
    pub self_agent_name: Option<String>,
    pub is_leader: Option<bool>,
    pub self_agent_color: Option<String>,
    pub teammates: BTreeMap<String, InboxPollerTeammateInfo>,
}

/// Maps to: CC `AppState.inbox.messages[number].status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxMessageStatus {
    Pending,
    Processing,
    Processed,
}

/// Maps to: CC `AppState.inbox.messages[number]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxQueuedMessage {
    pub id: String,
    pub from: String,
    pub text: String,
    pub timestamp: String,
    pub status: InboxMessageStatus,
    pub color: Option<String>,
    pub summary: Option<String>,
}

/// Maps to: CC `AppState.workerSandboxPermissions.queue[number]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSandboxPermissionRequest {
    pub request_id: String,
    pub worker_id: String,
    pub worker_name: String,
    pub worker_color: Option<String>,
    pub host: String,
    pub created_at: i64,
}

/// Pane kill side effect requested by leader shutdown approval handling.
/// Maps to: CC `useInboxPoller.ts` shutdown-approved branch calling
/// `backend.killPane(paneId)` after `ensureBackendsRegistered()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneKillRequest {
    pub agent_id: String,
    pub pane_id: String,
    pub backend_type: Option<String>,
    pub team_name: String,
}

/// Maps to: CC `AppState.inbox`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxState {
    pub messages: Vec<InboxQueuedMessage>,
}

/// Maps to: CC `AppState.workerSandboxPermissions`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSandboxPermissionsState {
    pub queue: Vec<WorkerSandboxPermissionRequest>,
    pub selected_index: usize,
}

/// Maps to: CC `AppState.pendingWorkerRequest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingWorkerRequest {
    pub tool_name: String,
    pub tool_use_id: String,
    pub description: String,
}

/// Maps to: CC `AppState.pendingSandboxRequest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSandboxRequest {
    pub request_id: String,
    pub host: String,
}

/// Maps to: CC REPL `setAppState` → `pendingSandboxRequest` while worker waits
/// for leader network approval.
/// P4 identity: fresh `Arc::new` per write mirrors CC's whole-object write
/// (REPL.tsx:2966-2973); the poll core stays by-value ([`InboxPollMutable`]).
pub fn set_pending_sandbox_request(
    store: &crate::state::store::AppStore,
    pending: Option<PendingSandboxRequest>,
) {
    store.replace_with(|state| {
        state.pending_sandbox_request = pending.map(std::sync::Arc::new);
    });
}

/// Maps to: CC clearing `pendingSandboxRequest` after leader response / abort.
pub fn clear_pending_sandbox_request(store: &crate::state::store::AppStore) {
    set_pending_sandbox_request(store, None);
}

/// Poller-local fields that are not AppState (permission confirm queue drains
/// into REPL `permission_queue`; pane kills are fire-and-forget side effects).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPollerState {
    /// Maps to CC REPL `ToolUseConfirmQueue` populated from pane-worker
    /// permission requests in `useInboxPoller.ts`.
    pub permission_queue: Vec<ToolUseConfirm>,
    pub pending_pane_kills: Vec<PaneKillRequest>,
    /// Maps to `isInProcessTeammate()` guard.  In-process teammates use
    /// `waitForNextPromptOrShutdown` in `utils/swarm/inProcessRunner.ts`, not
    /// this file-backed inbox poller.
    pub is_in_process_teammate: bool,
}

impl Default for InboxPollerState {
    fn default() -> Self {
        Self {
            permission_queue: Vec::new(),
            pending_pane_kills: Vec::new(),
            is_in_process_teammate: false,
        }
    }
}

/// Working set for one poll / deliver cycle.
///
/// AppState-owned bags (`inbox`, `workerSandboxPermissions`,
/// `pendingSandboxRequest`) are cloned in, mutated, then written back.
/// Poller-local fields stay on [`InboxPollerState`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InboxPollMutable {
    pub inbox: InboxState,
    pub worker_sandbox_permissions: WorkerSandboxPermissionsState,
    pub pending_sandbox_request: Option<PendingSandboxRequest>,
    pub poller: InboxPollerState,
}

impl InboxPollMutable {
    pub fn from_app_and_poller(
        inbox: InboxState,
        worker_sandbox_permissions: WorkerSandboxPermissionsState,
        pending_sandbox_request: Option<PendingSandboxRequest>,
        poller: InboxPollerState,
    ) -> Self {
        Self {
            inbox,
            worker_sandbox_permissions,
            pending_sandbox_request,
            poller,
        }
    }
}

/// Maps to: CC `AppState` fields touched by `useInboxPoller.ts` (legacy name
/// kept for call-site comments; prefer [`InboxPollMutable`]).
#[doc(hidden)]
pub type InboxPollerAppFields = InboxPollMutable;

/// Build the inbox-poller `teamContext` projection from the official-style
/// team file record maintained by TeamCreate/spawnMultiAgent.
/// Maps to: CC `AppState.teamContext` consumed by `useInboxPoller.ts`.
pub fn current_team_context_from_team_record() -> Option<InboxPollerTeamContext> {
    if let Some(record) = crate::utils::swarm::team_helpers::current_team_record() {
        let teammates: BTreeMap<String, InboxPollerTeammateInfo> = record
            .members
            .iter()
            .map(|member| {
                // Maps to: CC `TeamCreateTool.ts` / `spawnMultiAgent.ts`
                // `setAppState(... teamContext.teammates[id].color = assignTeammateColor(id))`.
                // Official team files do not require `color`, so this AppState
                // projection synthesizes the same stable in-memory color when
                // the persisted member record lacks one.
                let color = member.color.clone().or_else(|| {
                    Some(
                        crate::utils::swarm::teammate_layout_manager::assign_teammate_color(
                            &member.agent_id,
                        )
                        .official_name()
                        .to_string(),
                    )
                });
                (
                    member.agent_id.clone(),
                    InboxPollerTeammateInfo {
                        name: member.name.clone(),
                        agent_type: member.agent_type.clone(),
                        color,
                        tmux_session_name: teammate_session_name_from_member(member),
                        tmux_pane_id: (!member.tmux_pane_id.is_empty())
                            .then(|| member.tmux_pane_id.clone()),
                        cwd: (!member.cwd.is_empty()).then(|| member.cwd.clone()),
                        worktree_path: member.worktree_path.clone(),
                        spawned_at: Some(member.joined_at_ms),
                        backend_type: member.backend_type.clone(),
                    },
                )
            })
            .collect();
        let self_agent_color = teammates
            .get(&record.lead_agent_id)
            .and_then(|lead| lead.color.clone());
        return Some(InboxPollerTeamContext {
            team_name: record.team_name,
            team_file_path: record.team_file_path,
            lead_agent_id: record.lead_agent_id.clone(),
            self_agent_id: Some(record.lead_agent_id),
            self_agent_name: Some(crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()),
            is_leader: Some(true),
            self_agent_color,
            teammates,
        });
    }

    let dynamic = crate::utils::teammate::get_dynamic_team_context()?;
    let mut teammates = BTreeMap::new();
    teammates.insert(
        dynamic.agent_id.clone(),
        InboxPollerTeammateInfo {
            name: dynamic.agent_name.clone(),
            agent_type: None,
            color: dynamic.color.clone(),
            tmux_session_name: None,
            tmux_pane_id: None,
            cwd: None,
            worktree_path: None,
            spawned_at: None,
            backend_type: None,
        },
    );
    Some(InboxPollerTeamContext {
        team_name: dynamic.team_name,
        team_file_path: String::new(),
        lead_agent_id: String::new(),
        self_agent_id: Some(dynamic.agent_id),
        self_agent_name: Some(dynamic.agent_name),
        is_leader: Some(false),
        self_agent_color: dynamic.color,
        teammates,
    })
}

fn teammate_session_name_from_member(
    member: &crate::utils::swarm::team_helpers::TeamMemberRecord,
) -> Option<String> {
    // Maps to CC `AppState.teamContext.teammates[id].tmuxSessionName` values
    // assigned in TeamCreateTool.ts/spawnMultiAgent.ts. The official team file
    // does not persist tmuxSessionName, so projection restores deterministic
    // values for leader/in-process teammates and leaves pane sessions absent.
    if member.tmux_pane_id.is_empty() {
        return Some(String::new());
    }
    if member.backend_type.as_deref() == Some("in-process") {
        return Some("in-process".to_string());
    }
    None
}

/// Maps to: CC `useInboxPoller.ts#getAgentNameToPoll`.
pub fn get_agent_name_to_poll(
    poller: &InboxPollerState,
    team_context: Option<&InboxPollerTeamContext>,
) -> Option<String> {
    if poller.is_in_process_teammate {
        return None;
    }
    if crate::utils::teammate::is_teammate() {
        return crate::utils::teammate::get_agent_name();
    }
    if let Some(team_context) = team_context {
        if crate::utils::teammate::is_team_lead(Some(&team_context.lead_agent_id)) {
            return team_context
                .teammates
                .get(&team_context.lead_agent_id)
                .map(|teammate| teammate.name.clone())
                .or_else(|| Some(TEAM_LEAD_NAME.to_string()));
        }
    }
    None
}

/// Maps to: CC local arrays in `useInboxPoller.ts#poll` that separate mailbox
/// protocol messages from regular teammate messages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InboxMessageGroups {
    pub permission_requests: Vec<TeammateMessage>,
    pub permission_responses: Vec<TeammateMessage>,
    pub sandbox_permission_requests: Vec<TeammateMessage>,
    pub sandbox_permission_responses: Vec<TeammateMessage>,
    pub shutdown_requests: Vec<TeammateMessage>,
    pub shutdown_approvals: Vec<TeammateMessage>,
    pub team_permission_updates: Vec<TeammateMessage>,
    pub mode_set_requests: Vec<TeammateMessage>,
    pub plan_approval_requests: Vec<TeammateMessage>,
    pub plan_approval_responses: Vec<TeammateMessage>,
    pub regular_messages: Vec<TeammateMessage>,
}

/// Maps to: CC `useInboxPoller.ts#poll` message classification loop.
pub fn classify_inbox_messages(unread: &[TeammateMessage]) -> InboxMessageGroups {
    let mut groups = InboxMessageGroups::default();
    for message in unread {
        if is_permission_request(&message.text).is_some() {
            groups.permission_requests.push(message.clone());
        } else if is_permission_response(&message.text).is_some() {
            groups.permission_responses.push(message.clone());
        } else if is_sandbox_permission_request(&message.text).is_some() {
            groups.sandbox_permission_requests.push(message.clone());
        } else if is_sandbox_permission_response(&message.text).is_some() {
            groups.sandbox_permission_responses.push(message.clone());
        } else if is_shutdown_request(&message.text).is_some() {
            groups.shutdown_requests.push(message.clone());
        } else if is_shutdown_approved(&message.text).is_some() {
            groups.shutdown_approvals.push(message.clone());
        } else if is_team_permission_update(&message.text).is_some() {
            groups.team_permission_updates.push(message.clone());
        } else if is_mode_set_request(&message.text).is_some() {
            groups.mode_set_requests.push(message.clone());
        } else if is_plan_approval_request(&message.text).is_some() {
            groups.plan_approval_requests.push(message.clone());
        } else if is_plan_approval_response(&message.text).is_some() {
            groups.plan_approval_responses.push(message.clone());
        } else {
            groups.regular_messages.push(message.clone());
        }
    }
    groups
}

/// Maps to: CC `useInboxPoller.ts` regular-message XML wrapper.
pub fn format_teammate_messages(messages: &[InboxQueuedMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            let color_attr = message
                .color
                .as_ref()
                .map(|color| format!(" color=\"{}\"", escape_xml_attr(color)))
                .unwrap_or_default();
            let summary_attr = message
                .summary
                .as_ref()
                .map(|summary| format!(" summary=\"{}\"", escape_xml_attr(summary)))
                .unwrap_or_default();
            format!(
                "<{TEAMMATE_MESSAGE_TAG} teammate_id=\"{}\"{color_attr}{summary_attr}>\n{}\n</{TEAMMATE_MESSAGE_TAG}>",
                escape_xml_attr(&message.from),
                message.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn escape_xml_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn queued_from_teammate_message(message: &TeammateMessage) -> InboxQueuedMessage {
    InboxQueuedMessage {
        id: uuid::Uuid::new_v4().to_string(),
        from: message.from.clone(),
        text: message.text.clone(),
        timestamp: message.timestamp.clone(),
        status: InboxMessageStatus::Pending,
        color: message.color.clone(),
        summary: message.summary.clone(),
    }
}

/// Maps to: CC `useInboxPoller.ts#poll` props that affect delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxPollerConfig {
    pub enabled: bool,
    pub is_loading: bool,
    pub focused_input_dialog: Option<String>,
    /// Result returned by official `onSubmitMessage(formatted)`.  When false,
    /// the Rust core queues the messages exactly like the TS hook.
    pub idle_submit_accepted: bool,
}

impl Default for InboxPollerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            is_loading: false,
            focused_input_dialog: None,
            idle_submit_accepted: true,
        }
    }
}

/// Evidence/debug result for one poll.  Maps to the side effects performed in
/// `useInboxPoller.ts#poll`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InboxPollOutcome {
    pub unread_count: usize,
    pub marked_read: bool,
    pub queued_count: usize,
    pub submitted: Option<String>,
    pub permission_request_count: usize,
    pub permission_response_count: usize,
    pub sandbox_permission_request_count: usize,
    pub sandbox_permission_response_count: usize,
    pub shutdown_request_count: usize,
    pub shutdown_approval_count: usize,
    pub plan_approval_request_count: usize,
    pub plan_approval_response_count: usize,
    pub team_permission_update_count: usize,
    pub mode_set_request_count: usize,
}

/// Maps to: CC `useInboxPoller.ts#poll`.
/// `team_context` is AppState.teamContext (single source); mutated in-place when
/// shutdown approvals remove teammates.
/// AppState inbox / workerSandboxPermissions / pendingSandboxRequest ride on
/// [`InboxPollMutable`] and are written back by the REPL after the poll.
pub fn poll_inbox_once(
    state: &mut InboxPollMutable,
    team_context: &mut Option<InboxPollerTeamContext>,
    permission_context: &mut ToolPermissionContext,
    config: &InboxPollerConfig,
) -> InboxPollOutcome {
    if !config.enabled {
        return InboxPollOutcome::default();
    }

    let Some(agent_name) = get_agent_name_to_poll(&state.poller, team_context.as_ref()) else {
        return InboxPollOutcome::default();
    };
    let team_name_owned = team_context
        .as_ref()
        .map(|context| context.team_name.clone());
    let team_name = team_name_owned.as_deref();
    let unread = read_unread_messages(&agent_name, team_name);
    if unread.is_empty() {
        return InboxPollOutcome::default();
    }

    let mut groups = classify_inbox_messages(&unread);
    let is_teammate = crate::utils::teammate::is_teammate();
    let is_team_lead = team_context
        .as_ref()
        .is_some_and(|context| crate::utils::teammate::is_team_lead(Some(&context.lead_agent_id)));

    handle_plan_approval_responses(
        permission_context,
        &groups.plan_approval_responses,
        is_teammate,
    );
    handle_permission_requests(
        state,
        permission_context,
        team_context.as_ref(),
        &groups.permission_requests,
        is_team_lead,
    );
    handle_sandbox_permission_requests(state, &groups.sandbox_permission_requests, is_team_lead);
    handle_permission_responses(&groups.permission_responses, is_teammate);
    handle_sandbox_permission_responses(state, &groups.sandbox_permission_responses, is_teammate);
    handle_team_permission_updates(
        permission_context,
        &groups.team_permission_updates,
        is_teammate,
    );
    handle_mode_set_requests(
        permission_context,
        team_context.as_ref(),
        &groups.mode_set_requests,
        is_teammate,
    );
    handle_plan_approval_requests(
        permission_context,
        team_context.as_ref(),
        &groups.plan_approval_requests,
        is_team_lead,
        &mut groups.regular_messages,
    );
    handle_shutdown_requests(
        &groups.shutdown_requests,
        is_teammate,
        &mut groups.regular_messages,
    );
    handle_shutdown_approvals(
        state,
        team_context,
        &groups.shutdown_approvals,
        is_team_lead,
        &mut groups.regular_messages,
    );

    let mut outcome = InboxPollOutcome {
        unread_count: unread.len(),
        permission_request_count: groups.permission_requests.len(),
        permission_response_count: groups.permission_responses.len(),
        sandbox_permission_request_count: groups.sandbox_permission_requests.len(),
        sandbox_permission_response_count: groups.sandbox_permission_responses.len(),
        shutdown_request_count: groups.shutdown_requests.len(),
        shutdown_approval_count: groups.shutdown_approvals.len(),
        plan_approval_request_count: groups.plan_approval_requests.len(),
        plan_approval_response_count: groups.plan_approval_responses.len(),
        team_permission_update_count: groups.team_permission_updates.len(),
        mode_set_request_count: groups.mode_set_requests.len(),
        ..InboxPollOutcome::default()
    };

    if groups.regular_messages.is_empty() {
        outcome.marked_read = mark_messages_as_read(&agent_name, team_name);
        return outcome;
    }

    let queued_messages = groups
        .regular_messages
        .iter()
        .map(queued_from_teammate_message)
        .collect::<Vec<_>>();
    let formatted = format_teammate_messages(&queued_messages);
    if !config.is_loading && config.focused_input_dialog.is_none() {
        if config.idle_submit_accepted {
            outcome.submitted = Some(formatted);
        } else {
            outcome.queued_count = queued_messages.len();
            state.inbox.messages.extend(queued_messages);
        }
    } else {
        outcome.queued_count = queued_messages.len();
        state.inbox.messages.extend(queued_messages);
    }
    outcome.marked_read = mark_messages_as_read(&agent_name, team_name);
    outcome
}

/// Maps to: CC `useInboxPoller.ts` idle `useEffect` that drains pending inbox
/// messages and removes already processed messages.
pub fn deliver_pending_messages_when_idle(
    state: &mut InboxPollMutable,
    team_context: Option<&InboxPollerTeamContext>,
    enabled: bool,
    is_loading: bool,
    focused_input_dialog: Option<&str>,
    idle_submit_accepted: bool,
) -> Option<String> {
    if !enabled || is_loading || focused_input_dialog.is_some() {
        return None;
    }
    get_agent_name_to_poll(&state.poller, team_context)?;

    state
        .inbox
        .messages
        .retain(|message| message.status != InboxMessageStatus::Processed);
    let pending = state
        .inbox
        .messages
        .iter()
        .filter(|message| message.status == InboxMessageStatus::Pending)
        .cloned()
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return None;
    }
    let formatted = format_teammate_messages(&pending);
    if idle_submit_accepted {
        let pending_ids = pending
            .iter()
            .map(|message| message.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        state
            .inbox
            .messages
            .retain(|message| !pending_ids.contains(&message.id));
        Some(formatted)
    } else {
        None
    }
}

fn handle_plan_approval_responses(
    permission_context: &mut ToolPermissionContext,
    messages: &[TeammateMessage],
    is_teammate: bool,
) {
    if !is_teammate || !crate::utils::teammate::is_plan_mode_required() {
        return;
    }
    for message in messages {
        if message.from != TEAM_LEAD_NAME {
            continue;
        }
        let Some(parsed) = is_plan_approval_response(&message.text) else {
            continue;
        };
        if parsed.get("approved").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let target_mode = parsed
            .get("permissionMode")
            .and_then(Value::as_str)
            .and_then(external_permission_mode_from_string)
            .unwrap_or(PermissionMode::Default);
        let update = PermissionUpdate::SetMode {
            destination: PermissionUpdateDestination::Session,
            mode: target_mode,
        };
        *permission_context = apply_permission_update(permission_context, &update);
    }
}

fn handle_permission_requests(
    state: &mut InboxPollMutable,
    permission_context: &ToolPermissionContext,
    team_context: Option<&InboxPollerTeamContext>,
    messages: &[TeammateMessage],
    is_team_lead: bool,
) {
    if !is_team_lead {
        return;
    }
    let team_name = team_context.map(|context| context.team_name.clone());
    for message in messages {
        let Some(parsed) = is_permission_request(&message.text) else {
            continue;
        };
        let Some(tool_name) = parsed.get("tool_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(tool_use_id) = parsed.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        if state
            .poller
            .permission_queue
            .iter()
            .any(|queued| queued.tool_use_id() == tool_use_id)
        {
            continue;
        }
        let Some(request_id) = parsed.get("request_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(agent_id) = parsed.get("agent_id").and_then(Value::as_str) else {
            continue;
        };
        let description = parsed
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(tool_name);
        let input = parsed.get("input").cloned().unwrap_or(Value::Null);
        let input_summary = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
        let mut request =
            crate::utils::permissions::permissions::mock_permission_request_with_input(
                format!("perm-{tool_use_id}"),
                tool_use_id.to_string(),
                tool_name.to_string(),
                input_summary,
                input,
                permission_context.mode,
            );
        request.description = description.to_string();
        state.poller.permission_queue.push(
            ToolUseConfirm::new(request)
                .with_permission_prompt_start_time_ms(chrono::Utc::now().timestamp_millis())
                .with_worker_badge(PermissionWorkerBadge {
                    name: agent_id.to_string(),
                    color: Some("cyan".to_string()),
                })
                .with_mailbox_response_target(MailboxPermissionResponseTarget {
                    worker_name: agent_id.to_string(),
                    request_id: request_id.to_string(),
                    team_name: team_name.clone(),
                }),
        );
    }
}

fn handle_sandbox_permission_requests(
    state: &mut InboxPollMutable,
    messages: &[TeammateMessage],
    is_team_lead: bool,
) {
    if !is_team_lead {
        return;
    }
    for message in messages {
        let Some(parsed) = is_sandbox_permission_request(&message.text) else {
            continue;
        };
        let Some(host) = parsed.pointer("/hostPattern/host").and_then(Value::as_str) else {
            continue;
        };
        let Some(request_id) = parsed.get("requestId").and_then(Value::as_str) else {
            continue;
        };
        let Some(worker_id) = parsed.get("workerId").and_then(Value::as_str) else {
            continue;
        };
        let Some(worker_name) = parsed.get("workerName").and_then(Value::as_str) else {
            continue;
        };
        state
            .worker_sandbox_permissions
            .queue
            .push(WorkerSandboxPermissionRequest {
                request_id: request_id.to_string(),
                worker_id: worker_id.to_string(),
                worker_name: worker_name.to_string(),
                worker_color: parsed
                    .get("workerColor")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                host: host.to_string(),
                created_at: parsed
                    .get("createdAt")
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis().max(0)),
            });
    }
}

/// Maps to: CC useInboxPoller permission_response branch —
/// `processMailboxPermissionResponse` (pendingWorkerRequest cleared by callback).
fn handle_permission_responses(messages: &[TeammateMessage], is_teammate: bool) {
    if !is_teammate || messages.is_empty() {
        return;
    }
    for message in messages {
        let Some(parsed) = is_permission_response(&message.text) else {
            continue;
        };
        let Some(request_id) = parsed.get("request_id").and_then(Value::as_str) else {
            continue;
        };
        if !crate::hooks::use_swarm_permission_poller::has_permission_callback(request_id) {
            continue;
        }
        let subtype = parsed
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("error");
        if subtype == "success" {
            let updated_input = parsed.pointer("/response/updated_input").cloned();
            let permission_updates = parsed.pointer("/response/permission_updates").cloned();
            let _ = crate::hooks::use_swarm_permission_poller::process_mailbox_permission_response(
                request_id,
                "approved",
                None,
                updated_input,
                permission_updates.as_ref(),
            );
        } else {
            let feedback = parsed.get("error").and_then(Value::as_str);
            let _ = crate::hooks::use_swarm_permission_poller::process_mailbox_permission_response(
                request_id, "rejected", feedback, None, None,
            );
        }
    }
}

/// Maps to: CC useInboxPoller sandbox_permission_response branch —
/// `processSandboxPermissionResponse` + clear `pendingSandboxRequest`.
fn handle_sandbox_permission_responses(
    state: &mut InboxPollMutable,
    messages: &[TeammateMessage],
    is_teammate: bool,
) {
    if !is_teammate || messages.is_empty() {
        return;
    }
    for message in messages {
        let Some(parsed) = is_sandbox_permission_response(&message.text) else {
            continue;
        };
        let Some(request_id) = parsed.get("requestId").and_then(Value::as_str) else {
            continue;
        };
        let host = parsed
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let allow = parsed
            .get("allow")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if crate::hooks::use_swarm_permission_poller::has_sandbox_permission_callback(request_id) {
            let _ = crate::hooks::use_swarm_permission_poller::process_sandbox_permission_response(
                request_id, host, allow,
            );
        }
        if state
            .pending_sandbox_request
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            state.pending_sandbox_request = None;
        }
    }
}

fn handle_team_permission_updates(
    permission_context: &mut ToolPermissionContext,
    messages: &[TeammateMessage],
    is_teammate: bool,
) {
    if !is_teammate {
        return;
    }
    for message in messages {
        let Some(parsed) = is_team_permission_update(&message.text) else {
            continue;
        };
        let Some(update) = parse_team_permission_add_rules_update(&parsed) else {
            continue;
        };
        *permission_context = apply_permission_update(permission_context, &update);
    }
}

fn parse_team_permission_add_rules_update(parsed: &Value) -> Option<PermissionUpdate> {
    let update = parsed.get("permissionUpdate")?;
    if update.get("type").and_then(Value::as_str) != Some("addRules") {
        return None;
    }
    let behavior = match update.get("behavior").and_then(Value::as_str)? {
        "allow" => PermissionBehavior::Allow,
        "deny" => PermissionBehavior::Deny,
        "ask" => PermissionBehavior::Ask,
        _ => return None,
    };
    let destination = match update
        .get("destination")
        .and_then(Value::as_str)
        .unwrap_or("session")
    {
        "session" => PermissionUpdateDestination::Session,
        "localSettings" => PermissionUpdateDestination::LocalSettings,
        "userSettings" => PermissionUpdateDestination::UserSettings,
        "projectSettings" => PermissionUpdateDestination::ProjectSettings,
        "cliArg" => PermissionUpdateDestination::CliArg,
        _ => PermissionUpdateDestination::Session,
    };
    let rules = update
        .get("rules")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|rule| {
            let tool_name = rule.get("toolName").and_then(Value::as_str)?;
            let rule_content = rule
                .get("ruleContent")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(PermissionRuleValue::new(tool_name, rule_content))
        })
        .collect::<Vec<_>>();
    if rules.is_empty() {
        return None;
    }
    Some(PermissionUpdate::AddRules {
        destination,
        behavior,
        rules,
    })
}

fn handle_mode_set_requests(
    permission_context: &mut ToolPermissionContext,
    team_context: Option<&InboxPollerTeamContext>,
    messages: &[TeammateMessage],
    is_teammate: bool,
) {
    if !is_teammate {
        return;
    }
    for message in messages {
        if message.from != TEAM_LEAD_NAME {
            continue;
        }
        let Some(parsed) = is_mode_set_request(&message.text) else {
            continue;
        };
        let Some(mode_text) = parsed.get("mode").and_then(Value::as_str) else {
            continue;
        };
        let mode = permission_mode_from_string(mode_text);
        let update = PermissionUpdate::SetMode {
            destination: PermissionUpdateDestination::Session,
            mode,
        };
        *permission_context = apply_permission_update(permission_context, &update);
        if let (Some(team_name), Some(agent_name)) = (
            team_context.map(|context| context.team_name.as_str()),
            crate::utils::teammate::get_agent_name(),
        ) {
            let _ = crate::utils::swarm::team_helpers::set_member_mode(
                team_name,
                &agent_name,
                mode_text,
            );
        }
    }
}

fn handle_plan_approval_requests(
    permission_context: &ToolPermissionContext,
    team_context: Option<&InboxPollerTeamContext>,
    messages: &[TeammateMessage],
    is_team_lead: bool,
    regular_messages: &mut Vec<TeammateMessage>,
) {
    if !is_team_lead {
        return;
    }
    let team_name = team_context.map(|context| context.team_name.as_str());
    let leader_mode = to_external_permission_mode(permission_context.mode);
    let mode_to_inherit = if leader_mode == "plan" {
        "default"
    } else {
        leader_mode
    };
    for message in messages {
        let Some(parsed) = is_plan_approval_request(&message.text) else {
            continue;
        };
        let Some(request_id) = parsed.get("requestId").and_then(Value::as_str) else {
            continue;
        };
        let approval =
            create_plan_approval_response_message(request_id, true, None, Some(mode_to_inherit));
        match write_to_mailbox(
            &message.from,
            TeammateMessageInput {
                from: TEAM_LEAD_NAME.to_string(),
                text: approval.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                color: None,
                summary: None,
            },
            team_name,
        ) {
            Ok(()) => regular_messages.push(message.clone()),
            Err(error) => tracing::warn!(
                %error,
                recipient = %message.from,
                "failed to deliver plan approval response"
            ),
        }
    }
}

fn handle_shutdown_requests(
    messages: &[TeammateMessage],
    is_teammate: bool,
    regular_messages: &mut Vec<TeammateMessage>,
) {
    if is_teammate {
        regular_messages.extend(messages.iter().cloned());
    }
}

fn handle_shutdown_approvals(
    state: &mut InboxPollMutable,
    team_context: &mut Option<InboxPollerTeamContext>,
    messages: &[TeammateMessage],
    is_team_lead: bool,
    regular_messages: &mut Vec<TeammateMessage>,
) {
    if !is_team_lead {
        return;
    }
    for message in messages {
        let Some(parsed) = is_shutdown_approved(&message.text) else {
            continue;
        };
        let Some(teammate_name) = parsed.get("from").and_then(Value::as_str) else {
            continue;
        };
        if let Some(team_context) = team_context.as_mut() {
            if let Some((teammate_id, teammate)) = team_context
                .teammates
                .iter()
                .find(|(_, teammate)| teammate.name == teammate_name)
                .map(|(id, teammate)| (id.clone(), teammate.clone()))
            {
                let pane_id = parsed
                    .get("paneId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| teammate.tmux_pane_id.clone());
                let backend_type = parsed
                    .get("backendType")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| teammate.backend_type.clone());
                if let Some(pane_id) = pane_id {
                    state.poller.pending_pane_kills.push(PaneKillRequest {
                        agent_id: teammate_id.clone(),
                        pane_id,
                        backend_type,
                        team_name: team_context.team_name.clone(),
                    });
                }
                team_context.teammates.remove(&teammate_id);
                let _ = crate::utils::swarm::team_helpers::remove_member_by_agent_id(
                    &team_context.team_name,
                    &teammate_id,
                );
                // Maps to: CC useInboxPoller.ts:749-765 — inside the same
                // setAppState that drops the teammate from teamContext, mark
                // the teammate's in-process task completed so
                // hasRunningTeammates flips false and the spinner stops.
                // Wiring deviation (recorded): the Rust poll core is
                // by-value with no store access, so the tasks half runs
                // through the in-process registry + bound task store instead
                // of this poll's write-back transaction.
                crate::tasks::in_process_teammate_task::complete_in_process_teammate_tasks_for_agent(
                    &teammate_id,
                );
                state.inbox.messages.push(InboxQueuedMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    from: "system".to_string(),
                    text: serde_json::json!({
                        "type": "teammate_terminated",
                        "message": format!("{teammate_name} has shut down."),
                    })
                    .to_string(),
                    timestamp: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    status: InboxMessageStatus::Pending,
                    color: None,
                    summary: None,
                });
            }
        }
        regular_messages.push(message.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::teammate::{
        DynamicTeamContext, clear_dynamic_team_context, set_dynamic_team_context,
    };

    fn base_team_context() -> InboxPollerTeamContext {
        let mut teammates = BTreeMap::new();
        teammates.insert(
            "team-lead@alpha".to_string(),
            InboxPollerTeammateInfo {
                name: TEAM_LEAD_NAME.to_string(),
                color: Some("cyan".to_string()),
                ..InboxPollerTeammateInfo::default()
            },
        );
        InboxPollerTeamContext {
            team_name: "alpha".to_string(),
            team_file_path: "/tmp/alpha/config.json".to_string(),
            lead_agent_id: "team-lead@alpha".to_string(),
            self_agent_id: Some("team-lead@alpha".to_string()),
            self_agent_name: Some(TEAM_LEAD_NAME.to_string()),
            is_leader: Some(true),
            self_agent_color: Some("cyan".to_string()),
            teammates,
        }
    }

    fn base_team_state() -> (InboxPollMutable, Option<InboxPollerTeamContext>) {
        (InboxPollMutable::default(), Some(base_team_context()))
    }

    fn write_message(recipient: &str, from: &str, text: impl Into<String>) {
        write_to_mailbox(
            recipient,
            TeammateMessageInput {
                from: from.to_string(),
                text: text.into(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                color: Some("green".to_string()),
                summary: Some("summary".to_string()),
            },
            Some("alpha"),
        )
        .unwrap();
    }

    #[test]
    fn current_team_context_from_team_record_projects_official_team_file_shape() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        *crate::utils::swarm::team_helpers::TEAM_TOOL_STATE
            .lock()
            .unwrap() = None;
        crate::utils::swarm::teammate_layout_manager::clear_teammate_colors();
        let record = crate::utils::swarm::team_helpers::create_team_record(
            "alpha".to_string(),
            Some("review team".to_string()),
            Some("lead".to_string()),
            Some("sonnet".to_string()),
            "/tmp/project".to_string(),
        );
        let lead_agent_id = record.lead_agent_id.clone();
        crate::utils::swarm::team_helpers::write_team_record(record);

        let context = current_team_context_from_team_record().expect("team context");
        assert_eq!(context.team_name, "alpha");
        assert_eq!(context.lead_agent_id, lead_agent_id);
        assert_eq!(
            context.self_agent_id.as_deref(),
            Some(lead_agent_id.as_str())
        );
        assert_eq!(context.self_agent_name.as_deref(), Some(TEAM_LEAD_NAME));
        assert_eq!(context.is_leader, Some(true));
        assert_eq!(context.self_agent_color.as_deref(), Some("red"));
        let lead = context
            .teammates
            .get(&lead_agent_id)
            .expect("lead teammate");
        assert_eq!(lead.name, TEAM_LEAD_NAME);
        assert_eq!(lead.agent_type.as_deref(), Some("lead"));
        assert_eq!(lead.tmux_session_name.as_deref(), Some(""));
        assert_eq!(lead.cwd.as_deref(), Some("/tmp/project"));
        assert!(lead.spawned_at.is_some());
        assert_eq!(lead.color.as_deref(), Some("red"));

        *crate::utils::swarm::team_helpers::TEAM_TOOL_STATE
            .lock()
            .unwrap() = None;
        crate::utils::swarm::teammate_layout_manager::clear_teammate_colors();
    }

    #[test]
    fn current_team_context_falls_back_to_dynamic_teammate_context() {
        let _team_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        *crate::utils::swarm::team_helpers::TEAM_TOOL_STATE
            .lock()
            .unwrap() = None;
        clear_dynamic_team_context();
        set_dynamic_team_context(Some(DynamicTeamContext {
            agent_id: "reviewer@alpha".to_string(),
            agent_name: "reviewer".to_string(),
            team_name: "alpha".to_string(),
            color: Some("green".to_string()),
            plan_mode_required: false,
            parent_session_id: None,
        }));

        let context = current_team_context_from_team_record().expect("dynamic team context");
        assert_eq!(context.team_name, "alpha");
        assert_eq!(
            context
                .teammates
                .get("reviewer@alpha")
                .map(|teammate| (teammate.name.as_str(), teammate.color.as_deref())),
            Some(("reviewer", Some("green")))
        );
        clear_dynamic_team_context();
    }

    #[test]
    fn get_agent_name_to_poll_matches_official_teammate_and_leader_priority() {
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        clear_dynamic_team_context();
        assert_eq!(
            get_agent_name_to_poll(&InboxPollerState::default(), None),
            None
        );

        let (mut state, team_context) = base_team_state();
        assert_eq!(
            get_agent_name_to_poll(&state.poller, team_context.as_ref()),
            Some(TEAM_LEAD_NAME.to_string())
        );
        state.poller.is_in_process_teammate = true;
        assert_eq!(
            get_agent_name_to_poll(&state.poller, team_context.as_ref()),
            None
        );

        state.poller.is_in_process_teammate = false;
        set_dynamic_team_context(Some(DynamicTeamContext {
            agent_id: "reviewer@alpha".to_string(),
            agent_name: "reviewer".to_string(),
            team_name: "alpha".to_string(),
            color: Some("green".to_string()),
            plan_mode_required: false,
            parent_session_id: Some("parent".to_string()),
        }));
        assert_eq!(
            get_agent_name_to_poll(&state.poller, team_context.as_ref()),
            Some("reviewer".to_string())
        );
        clear_dynamic_team_context();
    }

    #[test]
    fn poll_queues_busy_regular_messages_then_delivers_when_idle() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext::default();
        write_message(TEAM_LEAD_NAME, "reviewer", "please inspect src/lib.rs");

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig {
                is_loading: true,
                ..InboxPollerConfig::default()
            },
        );
        assert_eq!(outcome.unread_count, 1);
        assert_eq!(outcome.queued_count, 1);
        assert!(outcome.marked_read);
        assert_eq!(state.inbox.messages.len(), 1);
        assert!(
            crate::utils::teammate_mailbox::read_unread_messages(TEAM_LEAD_NAME, Some("alpha"))
                .is_empty()
        );

        let delivered = deliver_pending_messages_when_idle(
            &mut state,
            team_context.as_ref(),
            true,
            false,
            None,
            true,
        )
        .expect("pending teammate message should submit");
        assert!(delivered.contains("<teammate-message teammate_id=\"reviewer\""));
        assert!(delivered.contains(" color=\"green\""));
        assert!(delivered.contains(" summary=\"summary\""));
        assert!(delivered.contains("please inspect src/lib.rs"));
        assert!(state.inbox.messages.is_empty());
    }

    #[test]
    fn refused_idle_submit_keeps_a_regular_message_queued_beside_a_permission_request() {
        // CC: when `onSubmitMessage` (REPL `handleIncomingPrompt`) refuses —
        // a typed prompt/bash command is queued — the teammate message stays
        // in the inbox for the idle delivery; it is never marked read and
        // dropped. The REPL answers that question before the poll.
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext::default();
        let request = crate::utils::teammate_mailbox::create_permission_request_message(
            "perm-1",
            "reviewer@alpha",
            "Bash",
            "toolu_perm",
            "Run tests",
            serde_json::json!({"command": "cargo test"}),
            Vec::new(),
        );
        write_message(TEAM_LEAD_NAME, "reviewer", request.to_string());
        write_message(TEAM_LEAD_NAME, "reviewer", "please inspect src/lib.rs");

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig {
                idle_submit_accepted: false,
                ..InboxPollerConfig::default()
            },
        );
        assert_eq!(outcome.permission_request_count, 1);
        assert!(outcome.submitted.is_none());
        assert_eq!(outcome.queued_count, 1);
        assert!(
            state
                .inbox
                .messages
                .iter()
                .any(|message| message.text.contains("please inspect src/lib.rs")),
            "the refused message waits in the inbox: {:?}",
            state.inbox.messages
        );
    }

    #[test]
    fn leader_permission_requests_queue_tool_use_confirm_with_worker_mailbox_target() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext::default();
        let request = crate::utils::teammate_mailbox::create_permission_request_message(
            "perm-1",
            "reviewer@alpha",
            "Bash",
            "toolu_perm",
            "Run tests",
            serde_json::json!({"command": "cargo test"}),
            Vec::new(),
        );
        write_message(TEAM_LEAD_NAME, "reviewer", request.to_string());

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig::default(),
        );
        assert_eq!(outcome.permission_request_count, 1);
        assert!(outcome.marked_read);
        assert_eq!(state.poller.permission_queue.len(), 1);
        let queued = &state.poller.permission_queue[0];
        assert_eq!(queued.request.tool_name, "Bash");
        assert_eq!(queued.request.tool_use_id, "toolu_perm");
        assert_eq!(queued.request.description, "Run tests");
        assert_eq!(
            queued
                .worker_badge
                .as_ref()
                .map(|badge| badge.name.as_str()),
            Some("reviewer@alpha")
        );
        assert_eq!(
            queued.mailbox_response_target.as_ref().map(|target| (
                target.worker_name.as_str(),
                target.request_id.as_str(),
                target.team_name.as_deref()
            )),
            Some(("reviewer@alpha", "perm-1", Some("alpha")))
        );
    }

    #[test]
    fn leader_shutdown_approval_queues_pane_kill_and_removes_teammate() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext::default();
        let context = team_context.as_mut().unwrap();
        context.teammates.insert(
            "reviewer@alpha".to_string(),
            InboxPollerTeammateInfo {
                name: "reviewer".to_string(),
                color: Some("green".to_string()),
                tmux_pane_id: Some("%42".to_string()),
                backend_type: Some("tmux".to_string()),
                ..InboxPollerTeammateInfo::default()
            },
        );
        let approval = crate::utils::teammate_mailbox::create_shutdown_approved_message(
            "shutdown-1",
            "reviewer",
            Some("%99"),
            Some("tmux"),
        );
        write_message(TEAM_LEAD_NAME, "reviewer", approval.to_string());

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig::default(),
        );
        assert_eq!(outcome.shutdown_approval_count, 1);
        assert!(
            !team_context
                .as_ref()
                .unwrap()
                .teammates
                .contains_key("reviewer@alpha")
        );
        assert_eq!(state.poller.pending_pane_kills.len(), 1);
        assert_eq!(
            state.poller.pending_pane_kills[0].agent_id,
            "reviewer@alpha"
        );
        assert_eq!(state.poller.pending_pane_kills[0].pane_id, "%99");
        assert_eq!(
            state.poller.pending_pane_kills[0].backend_type.as_deref(),
            Some("tmux")
        );
    }

    #[test]
    fn leader_auto_approves_plan_requests_and_submits_context_message() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext {
            mode: PermissionMode::Plan,
            ..ToolPermissionContext::default()
        };
        let request = crate::utils::teammate_mailbox::create_plan_approval_request_message(
            "reviewer",
            "plan-1",
            "/tmp/plan.md",
            "## Plan",
        );
        write_message(TEAM_LEAD_NAME, "reviewer", request.to_string());

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig::default(),
        );
        assert_eq!(outcome.plan_approval_request_count, 1);
        assert!(outcome.submitted.as_deref().is_some_and(|formatted| {
            formatted.contains("plan_approval_request")
                && formatted.contains("teammate_id=\"reviewer\"")
        }));
        let reviewer_messages =
            crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        assert_eq!(reviewer_messages.len(), 1);
        let approval = is_plan_approval_response(&reviewer_messages[0].text).unwrap();
        assert_eq!(
            approval.get("approved").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            approval.get("permissionMode").and_then(Value::as_str),
            Some("default")
        );
    }

    #[test]
    fn teammate_applies_mode_and_team_permission_updates() {
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        clear_dynamic_team_context();
        set_dynamic_team_context(Some(DynamicTeamContext {
            agent_id: "reviewer@alpha".to_string(),
            agent_name: "reviewer".to_string(),
            team_name: "alpha".to_string(),
            color: Some("green".to_string()),
            plan_mode_required: true,
            parent_session_id: Some("parent".to_string()),
        }));

        let (mut state, mut team_context) = base_team_state();
        let mut permission_context = ToolPermissionContext::default();
        let mode_set = crate::utils::teammate_mailbox::create_mode_set_request_message(
            "acceptEdits",
            TEAM_LEAD_NAME,
        );
        write_message("reviewer", TEAM_LEAD_NAME, mode_set.to_string());
        let team_update = serde_json::json!({
            "type": "team_permission_update",
            "permissionUpdate": {
                "type": "addRules",
                "rules": [{"toolName": "Edit", "ruleContent": "/tmp"}],
                "behavior": "allow",
                "destination": "session"
            },
            "directoryPath": "/tmp",
            "toolName": "Edit"
        });
        write_message("reviewer", TEAM_LEAD_NAME, team_update.to_string());
        let plan_response =
            create_plan_approval_response_message("plan-2", true, None, Some("dontAsk"));
        write_message("reviewer", TEAM_LEAD_NAME, plan_response.to_string());

        let outcome = poll_inbox_once(
            &mut state,
            &mut team_context,
            &mut permission_context,
            &InboxPollerConfig::default(),
        );
        assert_eq!(outcome.mode_set_request_count, 1);
        assert_eq!(outcome.team_permission_update_count, 1);
        assert_eq!(outcome.plan_approval_response_count, 1);
        // CC handles plan approval responses before mode-set requests in the
        // same poll pass, so the later team-lead mode change wins.
        assert_eq!(permission_context.mode, PermissionMode::AcceptEdits);
        let allow_rules = permission_context
            .always_allow_rules
            .get(&crate::types::permissions::PermissionRuleSource::Session)
            .cloned()
            .unwrap_or_default();
        assert!(allow_rules.contains(&PermissionRuleValue::new("Edit", Some("/tmp".to_string()))));
        clear_dynamic_team_context();
    }
}
