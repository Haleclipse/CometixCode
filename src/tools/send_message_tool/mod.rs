//! Incremental port of official `SendMessageTool/*`.
//!
//! Schema/prompt metadata maps to CC `tools/SendMessageTool/SendMessageTool.ts`
//! and `prompt.ts`. Local-agent routing maps to CC `SendMessageTool.ts`
//! background-agent branch; teammate routing now uses the official
//! `utils/teammateMailbox.ts` boundary (in-memory while team-file persistence is
//! disabled).

pub mod prompt;
pub mod ui;

/// Maps to: CC `utils/agentSwarmsEnabled.ts` `isAgentSwarmsEnabled()`.
pub fn is_send_message_tool_enabled() -> bool {
    crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
}

/// Maps to: CC `feature('UDS_INBOX')` guarding the cross-session address paths.
fn is_uds_inbox_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(crate::utils::feature_flags::FeatureFlag::UdsInbox)
}

/// Maps to: CC `SendMessageTool.ts:46-65` `StructuredMessage`.
fn structured_message_schema() -> crate::utils::zod::Schema {
    use crate::utils::zod;
    zod::discriminated_union(
        "type",
        vec![
            zod::object(vec![
                ("type", zod::literal(serde_json::json!("shutdown_request"))),
                ("reason", zod::string().optional()),
            ]),
            zod::object(vec![
                ("type", zod::literal(serde_json::json!("shutdown_response"))),
                ("request_id", zod::string()),
                (
                    "approve",
                    crate::utils::semantic_boolean::semantic_boolean_default(),
                ),
                ("reason", zod::string().optional()),
            ]),
            zod::object(vec![
                (
                    "type",
                    zod::literal(serde_json::json!("plan_approval_response")),
                ),
                ("request_id", zod::string()),
                (
                    "approve",
                    crate::utils::semantic_boolean::semantic_boolean_default(),
                ),
                ("feedback", zod::string().optional()),
            ]),
        ],
    )
}

/// Maps to: CC `SendMessageTool.ts:67-87` `inputSchema`.
///
/// `to`'s description branches on `feature('UDS_INBOX')` at schema-construction
/// time, exactly as CC does — `lazySchema()` freezes it for the session, and so
/// does this `OnceLock`.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod as zod;
        let to_description = if crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::UdsInbox,
        ) {
            "Recipient: teammate name, \"*\" for broadcast, \"uds:<socket-path>\" for a local peer, or \"bridge:<session-id>\" for a Remote Control peer (use ListPeers to discover)"
        } else {
            "Recipient: teammate name, or \"*\" for broadcast to all teammates"
        };
        zod::object(vec![
            ("to", zod::string().describe(to_description)),
            (
                "summary",
                zod::string().optional().describe(
                    "A 5-10 word summary shown as a preview in the UI (required when message is a string)",
                ),
            ),
            (
                "message",
                zod::union(vec![
                    zod::string().describe("Plain text message content"),
                    structured_message_schema(),
                ]),
            ),
        ])
    })
}

/// Maps to: CC `SendMessageTool` metadata.
pub fn send_message_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::SEND_MESSAGE_TOOL_NAME.to_string(),
        description: prompt::get_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/SendMessageTool/SendMessageTool.ts` `MessageRouting` (:82).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessageRoutingOutput {
    pub(crate) sender: Option<String>,
    pub(crate) sender_color: Option<String>,
    pub(crate) target: String,
    pub(crate) target_color: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) content: Option<String>,
}

/// CC `tools/SendMessageTool/SendMessageTool.ts` output union (:95-127).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SendMessageOutput {
    pub(crate) success: bool,
    pub(crate) message: String,
    pub(crate) recipients: Vec<String>,
    pub(crate) request_id: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) routing: Option<MessageRoutingOutput>,
}

/// Route a teammate message and return the official payload shape.
/// Maps to: CC `tools/SendMessageTool/SendMessageTool.ts` `call` (:741),
/// dispatching to `handleMessage`:149 / `handleBroadcast`:191 /
/// `handleShutdownRequest`:268 / `handlePlanApproval`:434 et al.
pub(crate) fn send_message_output(input: &serde_json::Value) -> SendMessageOutput {
    send_message_output_for_context(input, None)
}

async fn send_message_output_for_tool_call(
    input: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> SendMessageOutput {
    let recipient = input
        .get("to")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("unknown")
        .to_string();
    let plain_text = input
        .get("message")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if let Some(prompt) = plain_text.as_deref().filter(|_| recipient != "*") {
        match crate::tasks::local_agent_task::queue_pending_message(&recipient, prompt.to_string())
        {
            crate::tasks::local_agent_task::QueuePendingMessageResult::Queued => {
                return SendMessageOutput {
                    success: true,
                    message: format!(
                        "Message queued for delivery to {recipient} at its next tool round."
                    ),
                    recipients: Vec::new(),
                    request_id: None,
                    target: None,
                    routing: None,
                };
            }
            crate::tasks::local_agent_task::QueuePendingMessageResult::NotRunning { status } => {
                return resume_stopped_or_evicted_agent(
                    &recipient,
                    prompt,
                    context,
                    Some(status.as_str()),
                )
                .await;
            }
            crate::tasks::local_agent_task::QueuePendingMessageResult::NotFound => {
                if crate::types::ids::to_agent_id(&recipient).is_some() {
                    return resume_stopped_or_evicted_agent(&recipient, prompt, context, None)
                        .await;
                }
            }
        }
    }
    send_message_output_for_context(input, Some(context))
}

async fn resume_stopped_or_evicted_agent(
    recipient: &str,
    prompt: &str,
    context: &crate::tool::ToolUseContext,
    stopped_status: Option<&str>,
) -> SendMessageOutput {
    match crate::tools::agent_tool::resume_agent::resume_agent_background(
        crate::tools::agent_tool::resume_agent::ResumeAgentBackgroundParams {
            agent_id: recipient,
            prompt,
            tool_use_context: context,
            invoking_request_id: None,
        },
    )
    .await
    {
        Ok(result) => {
            let message = if let Some(status) = stopped_status {
                format!(
                    "Agent \"{recipient}\" was stopped ({status}); resumed it in the background with your message. You'll be notified when it finishes. Output: {}",
                    result.output_file
                )
            } else {
                format!(
                    "Agent \"{recipient}\" had no active task; resumed from transcript in the background with your message. You'll be notified when it finishes. Output: {}",
                    result.output_file
                )
            };
            SendMessageOutput {
                success: true,
                message,
                recipients: Vec::new(),
                request_id: None,
                target: None,
                routing: None,
            }
        }
        Err(error) => {
            let message = if let Some(status) = stopped_status {
                format!(
                    "Agent \"{recipient}\" is stopped ({status}) and could not be resumed: {error}"
                )
            } else {
                format!(
                    "Agent \"{recipient}\" is registered but has no transcript to resume. It may have been cleaned up. ({error})"
                )
            };
            SendMessageOutput {
                success: false,
                message,
                recipients: Vec::new(),
                request_id: None,
                target: Some(recipient.to_string()),
                routing: None,
            }
        }
    }
}

fn current_agent_id_for_context(context: Option<&crate::tool::ToolUseContext>) -> Option<String> {
    crate::utils::teammate::get_agent_id()
        .or_else(|| context.and_then(|context| context.agent_id.clone()))
}

fn current_team_name_for_context(context: Option<&crate::tool::ToolUseContext>) -> Option<String> {
    crate::utils::teammate::get_team_name(None)
        .or_else(|| {
            context
                .and_then(|context| context.agent_id.as_deref())
                .and_then(crate::utils::agent_id::parse_agent_id)
                .map(|(_, team_name)| team_name)
        })
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
}

fn sender_name_for_context(context: Option<&crate::tool::ToolUseContext>) -> String {
    crate::utils::teammate::get_agent_name()
        .or_else(|| {
            context
                .and_then(|context| context.agent_id.as_deref())
                .and_then(crate::utils::agent_id::parse_agent_id)
                .map(|(agent_name, _)| agent_name)
        })
        .unwrap_or_else(|| {
            if crate::utils::teammate::is_teammate() {
                "teammate".to_string()
            } else {
                crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()
            }
        })
}

/// Maps to: CC `SendMessageTool.ts:133-147` `findTeammateColor` — an
/// `appState.teamContext.teammates` read, NOT a team-file read; the
/// memory-first record is this port's carrier for that AppState shape.
fn teammate_color_by_name(team_name: Option<&str>, name: &str) -> Option<String> {
    let team_name = team_name?;
    crate::utils::swarm::team_helpers::memory_first_team_record(team_name).and_then(|record| {
        record
            .members
            .iter()
            .find(|member| member.name == name)
            .and_then(|member| member.color.clone())
    })
}

fn sender_color_for_context(team_name: Option<&str>, sender: &str) -> Option<String> {
    crate::utils::teammate::get_teammate_color()
        .or_else(|| teammate_color_by_name(team_name, sender))
}

fn write_teammate_mailbox(
    recipient: &str,
    from: &str,
    text: String,
    summary: Option<String>,
    color: Option<String>,
    team_name: Option<&str>,
) -> Result<(), String> {
    crate::utils::teammate_mailbox::write_to_mailbox(
        recipient,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: from.to_string(),
            text,
            // CC `new Date().toISOString()` — millisecond precision + `Z`.
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            color,
            summary,
        },
        team_name,
    )
}

fn mailbox_delivery_error(target: &str, error: String) -> SendMessageOutput {
    SendMessageOutput {
        success: false,
        message: format!("Failed to deliver message to {target}: {error}"),
        recipients: Vec::new(),
        request_id: None,
        target: Some(target.to_string()),
        routing: None,
    }
}

fn send_message_output_for_context(
    input: &serde_json::Value,
    context: Option<&crate::tool::ToolUseContext>,
) -> SendMessageOutput {
    use crate::utils::semantic_boolean::parse_json_bool;

    let recipient = input
        .get("to")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("unknown")
        .to_string();
    let summary = input
        .get("summary")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let message_value = input.get("message");
    let plain_text = message_value
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let structured_type = message_value
        .and_then(|value| value.get("type"))
        .and_then(|value| value.as_str());

    // Maps to CC `SendMessageTool.ts` local-agent routing branch before
    // ambient teammate mailbox routing.
    if structured_type.is_none() && recipient != "*" {
        match crate::tasks::local_agent_task::queue_pending_message(
            &recipient,
            plain_text.clone().unwrap_or_default(),
        ) {
            crate::tasks::local_agent_task::QueuePendingMessageResult::Queued => {
                return SendMessageOutput {
                    success: true,
                    message: format!(
                        "Message queued for delivery to {recipient} at its next tool round."
                    ),
                    recipients: Vec::new(),
                    request_id: None,
                    target: None,
                    routing: None,
                };
            }
            crate::tasks::local_agent_task::QueuePendingMessageResult::NotRunning { status } => {
                return SendMessageOutput {
                    success: false,
                    message: format!(
                        "Agent \"{recipient}\" is stopped ({status}) and could not be resumed: resume is unavailable in this context."
                    ),
                    recipients: Vec::new(),
                    request_id: None,
                    target: Some(recipient),
                    routing: None,
                };
            }
            crate::tasks::local_agent_task::QueuePendingMessageResult::NotFound => {
                if crate::types::ids::to_agent_id(&recipient).is_some() {
                    return SendMessageOutput {
                        success: false,
                        message: format!(
                            "Agent \"{recipient}\" had no active task; resume from transcript is unavailable in this context."
                        ),
                        recipients: Vec::new(),
                        request_id: None,
                        target: Some(recipient),
                        routing: None,
                    };
                }
            }
        }
    }

    let team_name = current_team_name_for_context(context);
    let sender = sender_name_for_context(context);
    let sender_color = sender_color_for_context(team_name.as_deref(), &sender);

    match structured_type {
        Some("shutdown_response") => {
            let request_id = message_value
                .and_then(|value| value.get("request_id"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let approved = message_value
                .and_then(|value| value.get("approve"))
                .and_then(parse_json_bool)
                .unwrap_or(false);
            let reason = message_value
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str())
                .unwrap_or("No reason provided");
            let agent_id = current_agent_id_for_context(context);
            let mut own_pane_id = None::<String>;
            let mut own_backend_type = None::<String>;
            if let (Some(team), Some(agent_id)) = (team_name.as_deref(), agent_id.as_deref()) {
                // CC :320 `readTeamFileAsync` — a pure disk read; teammate
                // processes update their member rows there.
                if let Some(record) = crate::utils::swarm::team_helpers::read_team_file(team) {
                    if let Some(member) = record
                        .members
                        .iter()
                        .find(|member| member.agent_id == agent_id)
                    {
                        own_pane_id =
                            (!member.tmux_pane_id.is_empty()).then(|| member.tmux_pane_id.clone());
                        own_backend_type = member.backend_type.clone();
                    }
                }
            }
            let message = if approved {
                crate::utils::teammate_mailbox::create_shutdown_approved_message(
                    request_id,
                    &sender,
                    own_pane_id.as_deref(),
                    own_backend_type.as_deref(),
                )
            } else {
                crate::utils::teammate_mailbox::create_shutdown_rejected_message(
                    request_id, &sender, reason,
                )
            };
            if let Err(error) = write_teammate_mailbox(
                crate::utils::swarm::constants::TEAM_LEAD_NAME,
                &sender,
                message.to_string(),
                None,
                sender_color.clone(),
                team_name.as_deref(),
            ) {
                return mailbox_delivery_error(
                    crate::utils::swarm::constants::TEAM_LEAD_NAME,
                    error,
                );
            }
            if approved {
                if own_backend_type.as_deref() == Some("in-process") {
                    if let Some(agent_id) = agent_id.as_deref() {
                        if let Some(task) =
                            crate::tasks::in_process_teammate_task::find_teammate_task_by_agent_id(
                                agent_id,
                            )
                        {
                            if let Some(abort) = task.abort_controller {
                                abort.abort();
                            }
                        }
                    }
                } else if let Some(agent_id) = agent_id.as_deref() {
                    if let Some(task) =
                        crate::tasks::in_process_teammate_task::find_teammate_task_by_agent_id(
                            agent_id,
                        )
                    {
                        if let Some(abort) = task.abort_controller {
                            abort.abort();
                        }
                    }
                }
            }
            SendMessageOutput {
                success: true,
                message: if approved {
                    format!(
                        "Shutdown approved. Sent confirmation to team-lead. Agent {sender} is now exiting."
                    )
                } else {
                    format!("Shutdown rejected. Reason: \"{reason}\". Continuing to work.")
                },
                recipients: Vec::new(),
                request_id: (!request_id.is_empty()).then(|| request_id.to_string()),
                target: None,
                routing: None,
            }
        }
        Some("plan_approval_response") => {
            let request_id = message_value
                .and_then(|value| value.get("request_id"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if sender != crate::utils::swarm::constants::TEAM_LEAD_NAME {
                return SendMessageOutput {
                    success: false,
                    message:
                        "Only the team lead can approve plans. Teammates cannot approve their own or other plans."
                            .to_string(),
                    recipients: Vec::new(),
                    request_id: (!request_id.is_empty()).then(|| request_id.to_string()),
                    target: Some(recipient),
                    routing: None,
                };
            }
            let approved = message_value
                .and_then(|value| value.get("approve"))
                .and_then(parse_json_bool)
                .unwrap_or(false);
            // CC defaults once at the dispatch site (:909 `feedback ??
            // 'Plan needs revision'`); the same value flows into both the
            // rejection JSON (:497) and the result copy (:514).
            let feedback = message_value
                .and_then(|value| value.get("feedback"))
                .and_then(|value| value.as_str())
                .unwrap_or("Plan needs revision");
            // CC handlePlanApproval :448-457 sends {type, requestId,
            // approved:true, timestamp, permissionMode: modeToInherit}
            // (leader 'plan' inherits as 'default'); rejection :493-499
            // carries feedback and no permissionMode.
            let response = if approved {
                let leader_mode = context
                    .and_then(|context| context.get_app_state())
                    .map(|state| state.tool_permission_context.mode)
                    .unwrap_or_else(|| {
                        context
                            .map(|context| context.tool_permission_context.mode)
                            .unwrap_or(crate::types::permissions::PermissionMode::Default)
                    });
                let mode_to_inherit =
                    if leader_mode == crate::types::permissions::PermissionMode::Plan {
                        crate::types::permissions::PermissionMode::Default
                    } else {
                        leader_mode
                    };
                crate::utils::teammate_mailbox::create_plan_approval_response_message(
                    request_id,
                    true,
                    None,
                    // CC :456 embeds the INTERNAL mode string ('auto' stays
                    // 'auto' on the wire; the receiver schema accepts it) —
                    // the external projection would collapse Auto → default.
                    Some(
                        crate::utils::permissions::permission_mode::permission_mode_internal_name(
                            mode_to_inherit,
                        ),
                    ),
                )
            } else {
                crate::utils::teammate_mailbox::create_plan_approval_response_message(
                    request_id,
                    false,
                    Some(feedback),
                    None,
                )
            };
            if let Err(error) = write_teammate_mailbox(
                &recipient,
                crate::utils::swarm::constants::TEAM_LEAD_NAME,
                response.to_string(),
                None,
                None,
                team_name.as_deref(),
            ) {
                return mailbox_delivery_error(&recipient, error);
            }
            SendMessageOutput {
                success: true,
                message: if approved {
                    format!(
                        "Plan approved for {recipient}. They will receive the approval and can proceed with implementation."
                    )
                } else {
                    format!("Plan rejected for {recipient} with feedback: \"{feedback}\"")
                },
                recipients: Vec::new(),
                request_id: (!request_id.is_empty()).then(|| request_id.to_string()),
                target: Some(recipient),
                routing: None,
            }
        }
        Some("shutdown_request") => {
            let request_id = crate::utils::agent_id::generate_request_id("shutdown", &recipient);
            let reason = message_value
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str());
            let shutdown_message = crate::utils::teammate_mailbox::create_shutdown_request_message(
                &request_id,
                &sender,
                reason,
            );
            if let Err(error) = write_teammate_mailbox(
                &recipient,
                &sender,
                shutdown_message.to_string(),
                None,
                sender_color.clone(),
                team_name.as_deref(),
            ) {
                return mailbox_delivery_error(&recipient, error);
            }
            SendMessageOutput {
                success: true,
                message: format!("Shutdown request sent to {recipient}. Request ID: {request_id}"),
                recipients: Vec::new(),
                request_id: Some(request_id),
                target: Some(recipient),
                routing: None,
            }
        }
        _ if recipient == "*" => {
            let Some(team) = team_name.as_deref() else {
                return SendMessageOutput {
                    success: false,
                    message: "Not in a team context. Create a team with Teammate spawnTeam first, or set CLAUDE_CODE_TEAM_NAME.".to_string(),
                    recipients: Vec::new(),
                    request_id: None,
                    target: None,
                    routing: None,
                };
            };
            // CC :205 `readTeamFileAsync` — the broadcast roster comes from
            // disk, where teammate processes register themselves.
            let Some(record) = crate::utils::swarm::team_helpers::read_team_file(team) else {
                return SendMessageOutput {
                    success: false,
                    message: format!("Team \"{team}\" does not exist"),
                    recipients: Vec::new(),
                    request_id: None,
                    target: None,
                    routing: None,
                };
            };
            let content = plain_text.unwrap_or_default();
            let recipients = record
                .members
                .iter()
                .filter(|member| member.name.to_lowercase() != sender.to_lowercase())
                .map(|member| member.name.clone())
                .collect::<Vec<_>>();
            if recipients.is_empty() {
                return SendMessageOutput {
                    success: true,
                    message: "No teammates to broadcast to (you are the only team member)"
                        .to_string(),
                    recipients,
                    request_id: None,
                    target: None,
                    routing: None,
                };
            }
            for recipient_name in &recipients {
                if let Err(error) = write_teammate_mailbox(
                    recipient_name,
                    &sender,
                    content.clone(),
                    summary.clone(),
                    sender_color.clone(),
                    Some(team),
                ) {
                    return mailbox_delivery_error(recipient_name, error);
                }
            }
            SendMessageOutput {
                success: true,
                message: format!(
                    "Message broadcast to {} teammate(s): {}",
                    recipients.len(),
                    recipients.join(", ")
                ),
                recipients,
                request_id: None,
                target: None,
                routing: Some(MessageRoutingOutput {
                    sender: Some(sender),
                    sender_color,
                    target: "@team".to_string(),
                    target_color: None,
                    summary,
                    content: Some(content),
                }),
            }
        }
        _ => {
            let content = plain_text.unwrap_or_default();
            if let Err(error) = write_teammate_mailbox(
                &recipient,
                &sender,
                content.clone(),
                summary.clone(),
                sender_color.clone(),
                team_name.as_deref(),
            ) {
                return mailbox_delivery_error(&recipient, error);
            }
            SendMessageOutput {
                success: true,
                message: format!("Message sent to {recipient}'s inbox"),
                recipients: Vec::new(),
                request_id: None,
                target: None,
                routing: Some(MessageRoutingOutput {
                    sender: Some(sender),
                    sender_color,
                    target: format!("@{recipient}"),
                    target_color: teammate_color_by_name(team_name.as_deref(), &recipient),
                    summary,
                    content: Some(content),
                }),
            }
        }
    }
}

pub(crate) fn send_message_output_json(output: &SendMessageOutput) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("success".to_string(), serde_json::json!(output.success));
    object.insert("message".to_string(), serde_json::json!(output.message));
    if !output.recipients.is_empty()
        || output
            .routing
            .as_ref()
            .is_some_and(|routing| routing.target == "*")
    {
        object.insert(
            "recipients".to_string(),
            serde_json::json!(output.recipients),
        );
    }
    if let Some(request_id) = &output.request_id {
        object.insert("request_id".to_string(), serde_json::json!(request_id));
    }
    if let Some(target) = &output.target {
        object.insert("target".to_string(), serde_json::json!(target));
    }
    if let Some(routing) = &output.routing {
        let mut routing_object = serde_json::Map::new();
        if let Some(sender) = &routing.sender {
            routing_object.insert("sender".to_string(), serde_json::json!(sender));
        }
        if let Some(sender_color) = &routing.sender_color {
            routing_object.insert("senderColor".to_string(), serde_json::json!(sender_color));
        }
        routing_object.insert("target".to_string(), serde_json::json!(routing.target));
        if let Some(target_color) = &routing.target_color {
            routing_object.insert("targetColor".to_string(), serde_json::json!(target_color));
        }
        if let Some(summary) = &routing.summary {
            routing_object.insert("summary".to_string(), serde_json::json!(summary));
        }
        if let Some(content) = &routing.content {
            routing_object.insert("content".to_string(), serde_json::json!(content));
        }
        object.insert(
            "routing".to_string(),
            serde_json::Value::Object(routing_object),
        );
    }
    serde_json::Value::Object(object)
}

/// Behavioral half of CC `SendMessageTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct SendMessageTool;

impl crate::tool::ToolCall for SendMessageTool {
    fn name(&self) -> &'static str {
        "SendMessage"
    }

    /// Maps to: CC `SendMessageTool.ts:724-726` `async prompt() { return
    /// getPrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::get_prompt()
    }

    /// Maps to: CC `SendMessageTool.ts:523` `searchHint`.
    fn search_hint(&self) -> Option<&'static str> {
        Some("send messages to agent teammates (swarm protocol)")
    }

    /// Maps to: CC `SendMessageTool.ts:524` `maxResultSizeChars`.
    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `SendMessageTool.ts:526-528` `userFacingName()`.
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        "SendMessage".to_string()
    }

    /// Maps to: CC `SendMessageTool.ts:533` `shouldDefer`.
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `SendMessageTool.ts:720-722` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        crate::tools::send_message_tool::prompt::DESCRIPTION.to_string()
    }

    fn is_enabled(&self) -> bool {
        is_send_message_tool_enabled()
    }

    /// Maps to: CC `SendMessageTool.ts:539-541` `isReadOnly(input)` — plain
    /// text is a mailbox write the permission system treats as read-only;
    /// structured protocol messages drive teammate lifecycle and are not.
    fn is_read_only(&self, args: &serde_json::Value) -> bool {
        args.get("message")
            .is_some_and(serde_json::Value::is_string)
    }

    /// Maps to: CC `SendMessageTool.ts:543-569` `backfillObservableInput`.
    fn backfill_observable_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> serde_json::Value {
        let mut backfilled = args.clone();
        let Some(object) = backfilled.as_object_mut() else {
            return backfilled;
        };
        if object.contains_key("type") {
            return backfilled;
        }
        let Some(recipient) = object
            .get("to")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        else {
            return backfilled;
        };
        let message = object.get("message").cloned();
        if recipient == "*" {
            object.insert("type".to_string(), serde_json::json!("broadcast"));
            if let Some(text) = message.as_ref().and_then(|value| value.as_str()) {
                object.insert("content".to_string(), serde_json::json!(text));
            }
        } else if let Some(text) = message.as_ref().and_then(|value| value.as_str()) {
            object.insert("type".to_string(), serde_json::json!("message"));
            object.insert("recipient".to_string(), serde_json::json!(recipient));
            object.insert("content".to_string(), serde_json::json!(text));
        } else if let Some(structured) = message.as_ref().and_then(|value| value.as_object()) {
            if let Some(message_type) = structured.get("type") {
                object.insert("type".to_string(), message_type.clone());
            }
            object.insert("recipient".to_string(), serde_json::json!(recipient));
            if let Some(request_id) = structured.get("request_id") {
                object.insert("request_id".to_string(), request_id.clone());
            }
            if let Some(approve) = structured.get("approve") {
                object.insert("approve".to_string(), approve.clone());
            }
            if let Some(content) = structured
                .get("reason")
                .or_else(|| structured.get("feedback"))
            {
                object.insert("content".to_string(), content.clone());
            }
        }
        backfilled
    }

    /// Maps to: CC `SendMessageTool.ts:571-583` `toAutoClassifierInput(input)`.
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let recipient = args
            .get("to")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let Some(message) = args.get("message") else {
            return String::new();
        };
        if let Some(text) = message.as_str() {
            return format!("to {recipient}: {text}");
        }
        let approved = message
            .get("approve")
            .and_then(crate::utils::semantic_boolean::parse_json_bool)
            .unwrap_or(false);
        let request_id = message
            .get("request_id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        match message.get("type").and_then(|value| value.as_str()) {
            Some("shutdown_request") => format!("shutdown_request to {recipient}"),
            Some("shutdown_response") => format!(
                "shutdown_response {} {request_id}",
                if approved { "approve" } else { "reject" }
            ),
            Some("plan_approval_response") => format!(
                "plan_approval {} to {recipient}",
                if approved { "approve" } else { "reject" }
            ),
            _ => String::new(),
        }
    }

    /// Maps to: CC `SendMessageTool.ts:585-602` `checkPermissions(...)`.
    ///
    /// The bridge consent prompt is a `safetyCheck`, not a mode decision:
    /// `permissions.ts` evaluates it ahead of both bypassPermissions (step 1g)
    /// and auto-mode's allowlist/classifier, so cross-machine prompt injection
    /// stays bypass-immune.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        let recipient = args
            .get("to")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if is_uds_inbox_enabled()
            && crate::utils::peer_address::parse_address(recipient).scheme
                == crate::utils::peer_address::PeerAddressScheme::Bridge
        {
            return crate::utils::permissions::permission_result::PermissionResult::Ask {
                message: format!(
                    "Send a message to Remote Control session {recipient}? It arrives as a user prompt on the receiving Claude (possibly another machine) via Anthropic's servers."
                ),
                updated_input: None,
                decision_reason: Some(
                    crate::utils::permissions::permission_result::PermissionDecisionReason::SafetyCheck {
                        reason: "Cross-machine bridge message requires explicit user consent"
                            .to_string(),
                        classifier_approvable: false,
                    },
                ),
                suggestions: Vec::new(),
                blocked_path: None,
                metadata: None,
                is_bash_security_check_for_misparsing: false,
                pending_classifier_check: None,
                content_blocks: Vec::new(),
            };
        }
        crate::utils::permissions::permission_result::PermissionResult::Allow {
            updated_input: Some(args.clone()),
            user_modified: None,
            decision_reason: None,
            tool_use_id: None,
            accept_feedback: None,
            content_blocks: Vec::new(),
        }
    }

    /// Maps to: CC `SendMessageTool.ts:604-718` `validateInput(...)`.
    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        use crate::tool::ValidationResult;
        use crate::utils::peer_address::{PeerAddressScheme, parse_address};

        let recipient = args
            .get("to")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if recipient.trim().is_empty() {
            return ValidationResult::error("to must not be empty", 9);
        }
        let address = parse_address(recipient);
        if matches!(
            address.scheme,
            PeerAddressScheme::Bridge | PeerAddressScheme::Uds
        ) && address.target.trim().is_empty()
        {
            return ValidationResult::error("address target must not be empty", 9);
        }
        if recipient.contains('@') {
            return ValidationResult::error(
                "to must be a bare teammate name or \"*\" — there is only one team per session",
                9,
            );
        }
        let message = args.get("message");
        let plain_text = message.and_then(|value| value.as_str());
        if is_uds_inbox_enabled() && address.scheme == PeerAddressScheme::Bridge {
            // Structured-message rejection first — it's the permanent
            // constraint, so the user isn't sent to reconnect only to hit it.
            if plain_text.is_none() {
                return ValidationResult::error(
                    "structured messages cannot be sent cross-session — only plain text",
                    9,
                );
            }
            // CC re-derives `from=` through `getReplBridgeHandle()`. No bridge
            // transport is ported, so the handle is permanently absent and this
            // is the branch a `bridge:` target reaches.
            return ValidationResult::error(
                "Remote Control is not connected — cannot send to a bridge: target. Reconnect with /remote-control first.",
                9,
            );
        }
        if is_uds_inbox_enabled()
            && address.scheme == PeerAddressScheme::Uds
            && plain_text.is_some()
        {
            // UDS cross-session sends render no summary, so none is required.
            return ValidationResult::Ok;
        }
        if plain_text.is_some() {
            let summary = args
                .get("summary")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if summary.trim().is_empty() {
                return ValidationResult::error("summary is required when message is a string", 9);
            }
            return ValidationResult::Ok;
        }

        if recipient == "*" {
            return ValidationResult::error(
                "structured messages cannot be broadcast (to: \"*\")",
                9,
            );
        }
        if is_uds_inbox_enabled() && address.scheme != PeerAddressScheme::Other {
            return ValidationResult::error(
                "structured messages cannot be sent cross-session — only plain text",
                9,
            );
        }

        let message_type = message
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str());
        if message_type == Some("shutdown_response") {
            let team_lead = crate::utils::swarm::constants::TEAM_LEAD_NAME;
            if recipient != team_lead {
                return ValidationResult::error(
                    format!("shutdown_response must be sent to \"{team_lead}\""),
                    9,
                );
            }
            let approved = message
                .and_then(|value| value.get("approve"))
                .and_then(crate::utils::semantic_boolean::parse_json_bool)
                .unwrap_or(false);
            let reason = message
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if !approved && reason.trim().is_empty() {
                return ValidationResult::error(
                    "reason is required when rejecting a shutdown request",
                    9,
                );
            }
        }

        ValidationResult::Ok
    }

    /// Maps to: CC `SendMessageTool.ts:46-64` `StructuredMessage` parsing.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        if let Some(message) = parsed.get_mut("message") {
            crate::utils::semantic_boolean::preprocess_object_field(message, "approve");
        }
        parsed
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::SendMessage(
                    send_message_output_for_tool_call(args, _context).await,
                ),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/SendMessageTool/SendMessageTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:728-739).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::SendMessage(output) => (
                send_message_output_json(output).to_string(),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>SendMessage returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC `tools/SendMessageTool/UI.tsx:19-39`
    /// `renderToolResultMessage` — a routed result (`'routing' in result &&
    /// result.routing`, :27-29) or a request/target pair (:31-33) returns
    /// null, so `UserToolSuccessMessage.tsx:103` renders nothing; only the
    /// plain-message shape renders (`<Text dimColor>{result.message}</Text>`,
    /// :35-38). The emit-UI gate drops the null-shape rows at render, on the
    /// live and cold paths alike (`success_tool_result_is_nonvisual`
    /// "sendmessage" → `ui::renders_result`).
    /// Maps to: CC recording SendMessageTool's output payload as the
    /// message's `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::SendMessage(output) => Some(send_message_output_json(output)),
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
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestStateLock};

    static SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK: std::sync::LazyLock<TestStateLock> =
        std::sync::LazyLock::new(TestStateLock::new);

    #[test]
    fn send_message_queues_plain_text_for_running_local_agent_like_official() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );

        let output = super::send_message_output(&serde_json::json!({
            "to": task_id,
            "summary": "continue tests",
            "message": "continue with tests"
        }));

        assert!(output.success);
        assert!(output.message.contains("queued for delivery"));
        assert_eq!(
            crate::tasks::local_agent_task::drain_pending_messages(&task_id),
            vec!["continue with tests".to_string()]
        );
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
    }

    #[test]
    fn send_message_reports_stopped_local_agent_resume_gap_explicitly() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        crate::tasks::local_agent_task::complete_agent_task(
            &crate::tools::agent_tool::agent_tool_utils::CompletedAgentRun {
                agent_id: task_id.clone(),
                agent_type: "general-purpose".to_string(),
                content: vec!["done".to_string()],
                messages: Vec::new(),
                total_tool_use_count: 0,
                total_duration_ms: 1,
                total_tokens: 2,
                usage: None,
                content_replacement_state: None,
            },
        );
        let output = super::send_message_output(&serde_json::json!({
            "to": task_id,
            "summary": "continue tests",
            "message": "continue with tests"
        }));
        assert!(!output.success);
        assert!(output.message.contains("could not be resumed"));
        crate::utils::message_queue_manager::clear_command_queue();
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
    }

    #[test]
    fn send_message_tool_call_attempts_official_resume_for_stopped_agent() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        crate::utils::message_queue_manager::clear_command_queue();
        crate::utils::task::disk_output::reset_task_output_dir_for_test();
        let task_id = crate::tools::agent_tool::run_agent::create_agent_id(None);
        let agent = crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
            "general-purpose",
            "Use for general tasks",
            crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::BuiltIn,
        );
        crate::tasks::local_agent_task::register_async_agent(
            crate::tasks::local_agent_task::RegisterAsyncAgentParams {
                agent_id: task_id.clone(),
                description: "inspect".to_string(),
                prompt: "read files".to_string(),
                selected_agent: agent,
                tool_use_id: None,
            },
        );
        crate::tasks::local_agent_task::complete_agent_task(
            &crate::tools::agent_tool::agent_tool_utils::CompletedAgentRun {
                agent_id: task_id.clone(),
                agent_type: "general-purpose".to_string(),
                content: vec!["done".to_string()],
                messages: Vec::new(),
                total_tool_use_count: 0,
                total_duration_ms: 1,
                total_tokens: 2,
                usage: None,
                content_replacement_state: None,
            },
        );
        let args = serde_json::json!({
            "to": task_id,
            "summary": "continue tests",
            "message": "continue with tests"
        });
        let request = crate::types::permissions::PermissionRequest {
            permission_result: None,
            id: "perm-send-message".to_string(),
            tool_use_id: "toolu_send".to_string(),
            tool_name: "SendMessage".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: String::new(),
            input: args.clone(),
            call_input: None,
            rule: crate::types::permissions::PermissionRuleValue::new("SendMessage", None),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: crate::types::permissions::PermissionMode::Default,
        };
        let result = futures::executor::block_on(crate::tool::ToolCall::call(
            &super::SendMessageTool,
            &args,
            &request,
            &crate::tool::ToolUseContext::default(),
            None,
            None,
            None,
        ));
        match result.data {
            crate::tool::ToolOutput::SendMessage(output) => {
                assert!(!output.success);
                assert!(output.message.contains("could not be resumed"));
                assert!(output.message.contains("No transcript found"));
            }
            _ => panic!("unexpected output variant"),
        }
        crate::utils::message_queue_manager::clear_command_queue();
        let _ = crate::utils::task::disk_output::cleanup_task_output(&task_id);
    }

    #[test]
    fn send_message_does_not_synthesize_success_for_raw_agent_id_without_task() {
        let _task_lock = crate::tasks::local_agent_task::TEST_LOCAL_AGENT_TASK_LOCK
            .lock()
            .unwrap();
        let _queue_lock = crate::utils::message_queue_manager::TEST_QUEUE_LOCK
            .lock()
            .unwrap();
        crate::tasks::local_agent_task::clear_local_agent_tasks_for_test();
        let agent_id = crate::tools::agent_tool::run_agent::create_agent_id(None);
        let output = super::send_message_output(&serde_json::json!({
            "to": agent_id,
            "summary": "continue tests",
            "message": "continue with tests"
        }));
        assert!(!output.success);
        assert!(
            output
                .message
                .contains("resume from transcript is unavailable")
        );
    }

    /// Seeds the team both in memory AND on disk: broadcast and
    /// shutdown-approval read the team file like CC `readTeamFileAsync`, so
    /// the returned guards (CLAUDE_CONFIG_DIR + COMETIX_TEST_TEAM_FILE_IO)
    /// must stay alive for the test body.
    fn seed_team_for_send_message(team_name: &str) -> (EnvVarGuard, EnvVarGuard, EnvVarGuard) {
        let root = std::env::temp_dir().join(format!(
            "cometix-send-message-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        // The mailbox write path also checks the session write gate.
        let write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let mut record = crate::utils::swarm::team_helpers::create_team_record(
            team_name.to_string(),
            None,
            Some(crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()),
            None,
            "/tmp".to_string(),
        );
        record
            .members
            .push(crate::utils::swarm::team_helpers::TeamMemberRecord {
                agent_id: format!("reviewer@{team_name}"),
                name: "reviewer".to_string(),
                agent_type: Some("general-purpose".to_string()),
                model: None,
                prompt: None,
                color: Some("green".to_string()),
                plan_mode_required: Some(false),
                joined_at_ms: 1,
                tmux_pane_id: "in-process".to_string(),
                cwd: "/tmp".to_string(),
                worktree_path: None,
                session_id: None,
                subscriptions: Vec::new(),
                backend_type: Some("in-process".to_string()),
                is_active: None,
                mode: None,
            });
        record
            .members
            .push(crate::utils::swarm::team_helpers::TeamMemberRecord {
                agent_id: format!("tester@{team_name}"),
                name: "tester".to_string(),
                agent_type: Some("general-purpose".to_string()),
                model: None,
                prompt: None,
                color: Some("blue".to_string()),
                plan_mode_required: Some(false),
                joined_at_ms: 1,
                tmux_pane_id: "in-process".to_string(),
                cwd: "/tmp".to_string(),
                worktree_path: None,
                session_id: None,
                subscriptions: Vec::new(),
                backend_type: Some("in-process".to_string()),
                is_active: None,
                mode: None,
            });
        crate::utils::swarm::team_helpers::write_team_record(record);
        (config_guard, io_guard, write_guard)
    }

    #[test]
    fn send_message_routes_plain_text_to_teammate_mailbox_like_official() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        let output = super::send_message_output(&serde_json::json!({
            "to": "reviewer",
            "summary": "review request",
            "message": "please review this patch"
        }));

        assert!(output.success);
        assert_eq!(output.message, "Message sent to reviewer's inbox");
        assert_eq!(output.routing.as_ref().unwrap().target, "@reviewer");
        assert_eq!(
            output.routing.as_ref().unwrap().target_color.as_deref(),
            Some("green")
        );
        let inbox = crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].from,
            crate::utils::swarm::constants::TEAM_LEAD_NAME
        );
        assert_eq!(inbox[0].text, "please review this patch");
        assert_eq!(inbox[0].summary.as_deref(), Some("review request"));
    }

    #[test]
    fn send_message_broadcasts_to_team_file_members_like_official() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        let output = super::send_message_output(&serde_json::json!({
            "to": "*",
            "summary": "team update",
            "message": "stand by for review"
        }));

        assert!(output.success);
        assert_eq!(
            output.recipients,
            vec!["reviewer".to_string(), "tester".to_string()]
        );
        assert!(
            output
                .message
                .contains("Message broadcast to 2 teammate(s)")
        );
        assert_eq!(
            crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"))[0].text,
            "stand by for review"
        );
        assert_eq!(
            crate::utils::teammate_mailbox::read_mailbox("tester", Some("alpha"))[0].text,
            "stand by for review"
        );
    }

    #[test]
    fn send_message_shutdown_request_writes_structured_mailbox_message() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        let output = super::send_message_output(&serde_json::json!({
            "to": "reviewer",
            "message": {"type":"shutdown_request", "reason":"done"}
        }));

        assert!(output.success);
        assert_ne!(output.request_id.as_deref(), Some("mock-request"));
        let inbox = crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        let parsed = crate::utils::teammate_mailbox::is_shutdown_request(&inbox[0].text).unwrap();
        assert_eq!(
            parsed["from"].as_str(),
            Some(crate::utils::swarm::constants::TEAM_LEAD_NAME)
        );
        assert_eq!(parsed["reason"].as_str(), Some("done"));
    }

    #[test]
    fn send_message_plan_approval_response_writes_to_recipient_mailbox() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        let output = super::send_message_output(&serde_json::json!({
            "to": "reviewer",
            "message": {"type":"plan_approval_response", "request_id":"plan-1", "approve": true}
        }));

        assert!(output.success);
        assert!(output.message.contains("Plan approved for reviewer"));
        let inbox = crate::utils::teammate_mailbox::read_mailbox("reviewer", Some("alpha"));
        let parsed: serde_json::Value = serde_json::from_str(&inbox[0].text).unwrap();
        assert_eq!(parsed["type"].as_str(), Some("plan_approval_response"));
        assert_eq!(parsed["requestId"].as_str(), Some("plan-1"));
        assert_eq!(parsed["approved"].as_bool(), Some(true));
    }

    #[test]
    fn send_message_uses_dynamic_pane_teammate_identity_for_mailbox_routing() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        crate::utils::teammate::clear_dynamic_team_context();
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "reviewer@alpha".to_string(),
                agent_name: "reviewer".to_string(),
                team_name: "alpha".to_string(),
                color: Some("green".to_string()),
                plan_mode_required: false,
                parent_session_id: Some("parent".to_string()),
            },
        ));

        let output = super::send_message_output(&serde_json::json!({
            "to": "tester",
            "summary": "status update",
            "message": "I found the failing test"
        }));

        assert!(output.success);
        assert_eq!(
            output.routing.as_ref().unwrap().sender.as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            output.routing.as_ref().unwrap().sender_color.as_deref(),
            Some("green")
        );
        let inbox = crate::utils::teammate_mailbox::read_mailbox("tester", Some("alpha"));
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "reviewer");
        assert_eq!(inbox[0].color.as_deref(), Some("green"));
        assert_eq!(inbox[0].text, "I found the failing test");

        crate::utils::teammate::clear_dynamic_team_context();
    }

    #[test]
    fn shutdown_response_from_pane_teammate_includes_pane_backend_for_leader_lifecycle() {
        let _lock = SEND_MESSAGE_TEAM_MAILBOX_TEST_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _guards = seed_team_for_send_message("alpha");
        let mut record =
            crate::utils::swarm::team_helpers::memory_first_team_record("alpha").unwrap();
        if let Some(member) = record
            .members
            .iter_mut()
            .find(|member| member.agent_id == "reviewer@alpha")
        {
            member.tmux_pane_id = "%7".to_string();
            member.backend_type = Some("tmux".to_string());
        }
        crate::utils::swarm::team_helpers::write_team_record(record);
        crate::utils::teammate::clear_dynamic_team_context();
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "reviewer@alpha".to_string(),
                agent_name: "reviewer".to_string(),
                team_name: "alpha".to_string(),
                color: Some("green".to_string()),
                plan_mode_required: false,
                parent_session_id: Some("parent".to_string()),
            },
        ));

        let output = super::send_message_output(&serde_json::json!({
            "to": "team-lead",
            "message": {"type":"shutdown_response", "request_id":"shutdown-1", "approve": true}
        }));

        assert!(output.success);
        let inbox = crate::utils::teammate_mailbox::read_mailbox("team-lead", Some("alpha"));
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "reviewer");
        let parsed = crate::utils::teammate_mailbox::is_shutdown_approved(&inbox[0].text).unwrap();
        assert_eq!(
            parsed.get("from").and_then(|value| value.as_str()),
            Some("reviewer")
        );
        assert_eq!(
            parsed.get("paneId").and_then(|value| value.as_str()),
            Some("%7")
        );
        assert_eq!(
            parsed.get("backendType").and_then(|value| value.as_str()),
            Some("tmux")
        );

        crate::utils::teammate::clear_dynamic_team_context();
    }

    #[test]
    fn send_message_tool_schema_matches_official_input_shape() {
        let schema = super::send_message_tool_schema();
        assert_eq!(schema.name, "SendMessage");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["to", "message"]))
        );
        assert_eq!(
            schema
                .input_schema
                .pointer("/properties/message/anyOf/1/anyOf/2/properties/type/const"),
            Some(&serde_json::json!("plan_approval_response"))
        );
        assert_eq!(
            schema
                .input_schema
                .pointer("/properties/message/anyOf/1/anyOf/1/required"),
            Some(&serde_json::json!(["type", "request_id", "approve"]))
        );
        assert_eq!(
            schema
                .input_schema
                .pointer("/properties/message/anyOf/1/anyOf/1/properties/approve/type"),
            Some(&serde_json::json!("boolean"))
        );

        let raw = serde_json::json!({
            "to": "lead",
            "message": {
                "type": "shutdown_response",
                "request_id": "req-1",
                "approve": "false"
            }
        });
        assert!(
            crate::services::tools::tool_execution::validate_tool_input_against_schema(
                &schema.name,
                &raw,
                &schema.input_schema,
            )
            .is_err()
        );
        let parsed = crate::tool::ToolCall::normalize_input(&super::SendMessageTool, &raw);
        assert_eq!(parsed["message"]["approve"], serde_json::json!(false));
        assert!(
            crate::services::tools::tool_execution::validate_tool_input_against_schema(
                &schema.name,
                &parsed,
                &schema.input_schema,
            )
            .is_ok()
        );
        assert!(
            crate::services::tools::tool_execution::validate_tool_input_against_schema(
                &schema.name,
                &serde_json::json!({
                    "to": "lead",
                    "message": {"type": "shutdown_response", "request_id": "req-1"}
                }),
                &schema.input_schema,
            )
            .is_err()
        );
        assert!(
            schema
                .description
                .contains("Send a message to another agent")
        );
    }
}
