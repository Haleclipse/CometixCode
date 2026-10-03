//! Teammate mailbox helpers.
//!
//! Maps to: CC `utils/teammateMailbox.ts`.
//!
//! Official Claude Code persists inbox JSON files under
//! `~/.claude/teams/{team}/inboxes/{agent}.json` with file locking. Cometix
//! now mirrors that file lifecycle for pane-backed teammates while retaining an
//! in-memory cache for the current in-process runner.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Maps to: CC `utils/teammateMailbox.ts#TeammateMessage`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeammateMessage {
    pub from: String,
    pub text: String,
    pub timestamp: String,
    pub read: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Input shape for `write_to_mailbox`; maps to `Omit<TeammateMessage, 'read'>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeammateMessageInput {
    pub from: String,
    pub text: String,
    pub timestamp: String,
    pub color: Option<String>,
    pub summary: Option<String>,
}

static MAILBOXES: LazyLock<Mutex<HashMap<(String, String), Vec<TeammateMessage>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn default_team_name(team_name: Option<&str>) -> String {
    team_name
        .filter(|team| !team.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
        .unwrap_or_else(|| "default".to_string())
}

fn maybe_skip_disk_io_in_tests() -> bool {
    #[cfg(test)]
    {
        !crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("COMETIX_TEST_TEAM_FILE_IO").as_deref(),
        )
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn inbox_path_buf(agent_name: &str, team_name: Option<&str>) -> PathBuf {
    let team = default_team_name(team_name);
    crate::utils::env_utils::get_teams_dir()
        .join(crate::utils::tasks::sanitize_path_component(&team))
        .join("inboxes")
        .join(format!(
            "{}.json",
            crate::utils::tasks::sanitize_path_component(agent_name)
        ))
}

/// Maps to: CC `teammateMailbox.ts#getInboxPath` path shape.
pub fn get_inbox_path(agent_name: &str, team_name: Option<&str>) -> String {
    inbox_path_buf(agent_name, team_name).display().to_string()
}

fn read_mailbox_from_disk(
    agent_name: &str,
    team_name: Option<&str>,
) -> Option<Vec<TeammateMessage>> {
    let path = inbox_path_buf(agent_name, team_name);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<TeammateMessage>>(&content).ok()
}

fn write_mailbox_to_disk(
    agent_name: &str,
    team_name: Option<&str>,
    messages: &[TeammateMessage],
) -> Result<(), String> {
    let path = inbox_path_buf(agent_name, team_name);
    let dir = path
        .parent()
        .ok_or_else(|| format!("Invalid inbox path: {}", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("Failed to create inbox directory {}: {err}", dir.display()))?;
    let content = serde_json::to_string_pretty(messages)
        .map_err(|err| format!("Failed to serialize inbox {}: {err}", path.display()))?;
    std::fs::write(&path, content)
        .map_err(|err| format!("Failed to write inbox {}: {err}", path.display()))
}

struct MailboxFileLock {
    path: PathBuf,
}

impl Drop for MailboxFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_mailbox_lock(inbox_path: &Path) -> Result<MailboxFileLock, String> {
    let lock_path = PathBuf::from(format!("{}.lock", inbox_path.display()));
    for attempt in 0..=10 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(MailboxFileLock { path: lock_path }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt < 10 => {
                let delay_ms = (5_u64 * (attempt + 1)).min(100);
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(err) => {
                return Err(format!(
                    "Failed to acquire inbox lock {}: {err}",
                    lock_path.display()
                ));
            }
        }
    }
    Err(format!(
        "Failed to acquire inbox lock {} after retries",
        lock_path.display()
    ))
}

/// Maps to: CC `teammateMailbox.ts#readMailbox`.
pub fn read_mailbox(agent_name: &str, team_name: Option<&str>) -> Vec<TeammateMessage> {
    if !maybe_skip_disk_io_in_tests() {
        if let Some(messages) = read_mailbox_from_disk(agent_name, team_name) {
            return messages;
        }
    }
    let team = default_team_name(team_name);
    MAILBOXES
        .lock()
        .unwrap()
        .get(&(team, agent_name.to_string()))
        .cloned()
        .unwrap_or_default()
}

/// Maps to: CC `teammateMailbox.ts#readUnreadMessages`.
pub fn read_unread_messages(agent_name: &str, team_name: Option<&str>) -> Vec<TeammateMessage> {
    read_mailbox(agent_name, team_name)
        .into_iter()
        .filter(|message| !message.read)
        .collect()
}

/// Maps to: CC `teammateMailbox.ts#writeToMailbox`.
///
/// Unlike the upstream best-effort logging boundary, the Rust API returns the
/// write failure to model-facing callers so they cannot report delivery when
/// no inbox mutation reached disk. In-memory state is committed only after the
/// durable write succeeds, preserving one success boundary for pane and
/// in-process teammates.
pub fn write_to_mailbox(
    recipient_name: &str,
    message: TeammateMessageInput,
    team_name: Option<&str>,
) -> Result<(), String> {
    let team = default_team_name(team_name);
    let new_message = TeammateMessage {
        from: message.from,
        text: message.text,
        timestamp: message.timestamp,
        read: false,
        color: message.color,
        summary: message.summary,
    };

    if maybe_skip_disk_io_in_tests() {
        MAILBOXES
            .lock()
            .unwrap()
            .entry((team, recipient_name.to_string()))
            .or_default()
            .push(new_message);
        return Ok(());
    }
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(crate::tools::shared::write_gate::MAILBOX_DELIVERY_DISABLED_ERROR.to_string());
    }

    let path = inbox_path_buf(recipient_name, Some(&team));
    let dir = path
        .parent()
        .ok_or_else(|| format!("Invalid inbox path: {}", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "Failed to create inbox directory {}: {error}",
            dir.display()
        )
    })?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(b"[]").map_err(|error| {
                format!("Failed to initialize inbox {}: {error}", path.display())
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "Failed to initialize inbox {}: {error}",
                path.display()
            ));
        }
    }

    let _lock = acquire_mailbox_lock(&path)?;
    let mut messages = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<Vec<TeammateMessage>>(&content)
            .map_err(|error| format!("Failed to parse inbox {}: {error}", path.display()))?,
        Err(error) => {
            return Err(format!("Failed to read inbox {}: {error}", path.display()));
        }
    };
    messages.push(new_message.clone());
    write_mailbox_to_disk(recipient_name, Some(&team), &messages)?;

    MAILBOXES
        .lock()
        .unwrap()
        .entry((team, recipient_name.to_string()))
        .or_default()
        .push(new_message);
    Ok(())
}

/// Maps to: CC `teammateMailbox.ts#markMessageAsReadByIndex`.
pub fn mark_message_as_read_by_index(
    agent_name: &str,
    team_name: Option<&str>,
    index: usize,
) -> bool {
    let team = default_team_name(team_name);
    let mut changed = false;
    {
        let mut mailboxes = MAILBOXES.lock().unwrap();
        if let Some(messages) = mailboxes.get_mut(&(team.clone(), agent_name.to_string())) {
            if let Some(message) = messages.get_mut(index) {
                message.read = true;
                changed = true;
            }
        }
    }

    if maybe_skip_disk_io_in_tests() {
        return changed;
    }

    let path = inbox_path_buf(agent_name, Some(&team));
    let Ok(_lock) = acquire_mailbox_lock(&path) else {
        return changed;
    };
    let mut messages = read_mailbox_from_disk(agent_name, Some(&team)).unwrap_or_default();
    let Some(message) = messages.get_mut(index) else {
        return changed;
    };
    message.read = true;
    let _ = write_mailbox_to_disk(agent_name, Some(&team), &messages);
    true
}

/// Maps to: CC `teammateMailbox.ts#markMessagesAsRead`.
pub fn mark_messages_as_read(agent_name: &str, team_name: Option<&str>) -> bool {
    let team = default_team_name(team_name);
    let mut changed = false;
    {
        let mut mailboxes = MAILBOXES.lock().unwrap();
        if let Some(messages) = mailboxes.get_mut(&(team.clone(), agent_name.to_string())) {
            for message in messages {
                if !message.read {
                    message.read = true;
                    changed = true;
                }
            }
        }
    }

    if maybe_skip_disk_io_in_tests() {
        return changed;
    }

    let path = inbox_path_buf(agent_name, Some(&team));
    let Ok(_lock) = acquire_mailbox_lock(&path) else {
        return changed;
    };
    let mut messages = read_mailbox_from_disk(agent_name, Some(&team)).unwrap_or_default();
    if messages.is_empty() {
        return changed;
    }
    for message in &mut messages {
        if !message.read {
            message.read = true;
            changed = true;
        }
    }
    let _ = write_mailbox_to_disk(agent_name, Some(&team), &messages);
    changed
}

/// Maps to: CC `teammateMailbox.ts#clearMailbox`.
pub fn clear_mailbox(agent_name: &str, team_name: Option<&str>) {
    let team = default_team_name(team_name);
    MAILBOXES
        .lock()
        .unwrap()
        .insert((team.clone(), agent_name.to_string()), Vec::new());
    if maybe_skip_disk_io_in_tests() {
        return;
    }
    let path = inbox_path_buf(agent_name, Some(&team));
    if path.exists() {
        let _ = write_mailbox_to_disk(agent_name, Some(&team), &[]);
    }
}

/// Maps to: CC `teammateMailbox.ts#formatTeammateMessages`.
pub fn format_teammate_messages(messages: &[TeammateMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            let color_attr = message
                .color
                .as_ref()
                .map(|color| format!(" color=\"{color}\""))
                .unwrap_or_default();
            let summary_attr = message
                .summary
                .as_ref()
                .map(|summary| format!(" summary=\"{summary}\""))
                .unwrap_or_default();
            format!(
                "<{} teammate_id=\"{}\"{}{}>\n{}\n</{}>",
                crate::constants::xml::TEAMMATE_MESSAGE_TAG,
                message.from,
                color_attr,
                summary_attr,
                message.text,
                crate::constants::xml::TEAMMATE_MESSAGE_TAG
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Maps to: CC `teammateMailbox.ts#createPermissionRequestMessage`.
pub fn create_permission_request_message(
    request_id: &str,
    agent_id: &str,
    tool_name: &str,
    tool_use_id: &str,
    description: &str,
    input: Value,
    permission_suggestions: Vec<Value>,
) -> Value {
    serde_json::json!({
        "type": "permission_request",
        "request_id": request_id,
        "agent_id": agent_id,
        "tool_name": tool_name,
        "tool_use_id": tool_use_id,
        "description": description,
        "input": input,
        "permission_suggestions": permission_suggestions,
    })
}

/// Maps to: CC `teammateMailbox.ts#createPermissionResponseMessage`.
pub fn create_permission_response_message(
    request_id: &str,
    subtype: &str,
    error: Option<&str>,
    updated_input: Option<Value>,
    permission_updates: Option<Vec<Value>>,
) -> Value {
    if subtype == "error" {
        return serde_json::json!({
            "type": "permission_response",
            "request_id": request_id,
            "subtype": "error",
            "error": error.unwrap_or("Permission denied"),
        });
    }
    serde_json::json!({
        "type": "permission_response",
        "request_id": request_id,
        "subtype": "success",
        "response": {
            "updated_input": updated_input,
            "permission_updates": permission_updates,
        }
    })
}

/// Maps to: CC `teammateMailbox.ts#isPermissionRequest`.
pub fn is_permission_request(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("permission_request")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isPermissionResponse`.
pub fn is_permission_response(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("permission_response")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#createSandboxPermissionRequestMessage`.
pub fn create_sandbox_permission_request_message(
    request_id: &str,
    worker_id: &str,
    worker_name: &str,
    worker_color: Option<&str>,
    host: &str,
) -> Value {
    serde_json::json!({
        "type": "sandbox_permission_request",
        "requestId": request_id,
        "workerId": worker_id,
        "workerName": worker_name,
        "workerColor": worker_color,
        "hostPattern": { "host": host },
        "createdAt": chrono::Utc::now().timestamp_millis().max(0),
    })
}

/// Maps to: CC `teammateMailbox.ts#createSandboxPermissionResponseMessage`.
pub fn create_sandbox_permission_response_message(
    request_id: &str,
    host: &str,
    allow: bool,
) -> Value {
    serde_json::json!({
        "type": "sandbox_permission_response",
        "requestId": request_id,
        "host": host,
        "allow": allow,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

/// Maps to: CC `teammateMailbox.ts#isSandboxPermissionRequest`.
pub fn is_sandbox_permission_request(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("sandbox_permission_request"))
        .then_some(parsed)
}

/// Maps to: CC `utils/teammateMailbox.ts#createIdleNotification`.
pub fn create_idle_notification(
    agent_id: &str,
    idle_reason: Option<&str>,
    summary: Option<&str>,
    completed_task_id: Option<&str>,
    completed_status: Option<&str>,
    failure_reason: Option<&str>,
) -> Value {
    serde_json::json!({
        "type": "idle_notification",
        "from": agent_id,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "idleReason": idle_reason,
        "summary": summary,
        "completedTaskId": completed_task_id,
        "completedStatus": completed_status,
        "failureReason": failure_reason,
    })
}

/// Maps to: CC `utils/teammateMailbox.ts#isIdleNotification`.
pub fn is_idle_notification(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("idle_notification")).then_some(parsed)
}

/// Maps to: CC `utils/teammateMailbox.ts#getLastPeerDmSummary`.
pub fn get_last_peer_dm_summary(messages: &[crate::types::message::Message]) -> Option<String> {
    for message in messages.iter().rev() {
        match message {
            crate::types::message::Message::User(user) => {
                let is_plain_user_prompt = user.content.iter().any(|content| {
                    matches!(
                        content,
                        crate::types::message::UserContent::Text(_)
                            | crate::types::message::UserContent::MetaText(_)
                    )
                });
                if is_plain_user_prompt {
                    break;
                }
            }
            crate::types::message::Message::Assistant(assistant) => {
                for block in &assistant.content {
                    let crate::types::message::AssistantContent::ToolUse(tool_use) = block else {
                        continue;
                    };
                    if tool_use.name
                        != crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME
                    {
                        continue;
                    }
                    let Some(to) = tool_use.input.get("to").and_then(Value::as_str) else {
                        continue;
                    };
                    if to == "*"
                        || to.eq_ignore_ascii_case(crate::utils::swarm::constants::TEAM_LEAD_NAME)
                    {
                        continue;
                    }
                    let Some(message) = tool_use.input.get("message").and_then(Value::as_str)
                    else {
                        continue;
                    };
                    let summary = tool_use
                        .input
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| message.chars().take(80).collect::<String>());
                    return Some(format!("[to {to}] {summary}"));
                }
            }
            _ => {}
        }
    }
    None
}

/// Maps to: CC `teammateMailbox.ts#createShutdownRequestMessage`.
pub fn create_shutdown_request_message(
    request_id: &str,
    from: &str,
    reason: Option<&str>,
) -> Value {
    serde_json::json!({
        "type": "shutdown_request",
        "requestId": request_id,
        "from": from,
        "reason": reason,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

/// Maps to: CC `teammateMailbox.ts#createShutdownApprovedMessage`.
pub fn create_shutdown_approved_message(
    request_id: &str,
    from: &str,
    pane_id: Option<&str>,
    backend_type: Option<&str>,
) -> Value {
    serde_json::json!({
        "type": "shutdown_approved",
        "requestId": request_id,
        "from": from,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "paneId": pane_id,
        "backendType": backend_type,
    })
}

/// Maps to: CC `teammateMailbox.ts#createShutdownRejectedMessage`.
pub fn create_shutdown_rejected_message(request_id: &str, from: &str, reason: &str) -> Value {
    serde_json::json!({
        "type": "shutdown_rejected",
        "requestId": request_id,
        "from": from,
        "reason": reason,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

/// Maps to: CC `teammateMailbox.ts#isShutdownRequest`.
pub fn is_shutdown_request(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("shutdown_request")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isShutdownApproved`.
pub fn is_shutdown_approved(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("shutdown_approved")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isShutdownRejected`.
pub fn is_shutdown_rejected(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("shutdown_rejected")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isSandboxPermissionResponse`.
pub fn is_sandbox_permission_response(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("sandbox_permission_response"))
        .then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#PlanApprovalRequestMessageSchema` shape.
pub fn create_plan_approval_request_message(
    from: &str,
    request_id: &str,
    plan_file_path: &str,
    plan_content: &str,
) -> Value {
    serde_json::json!({
        "type": "plan_approval_request",
        "from": from,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "planFilePath": plan_file_path,
        "planContent": plan_content,
        "requestId": request_id,
    })
}

/// Maps to: CC `teammateMailbox.ts#isPlanApprovalRequest`.
pub fn is_plan_approval_request(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    let valid = parsed.get("type").and_then(Value::as_str) == Some("plan_approval_request")
        && parsed.get("from").and_then(Value::as_str).is_some()
        && parsed.get("timestamp").and_then(Value::as_str).is_some()
        && parsed.get("planFilePath").and_then(Value::as_str).is_some()
        && parsed.get("planContent").and_then(Value::as_str).is_some()
        && parsed.get("requestId").and_then(Value::as_str).is_some();
    valid.then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#PlanApprovalResponseMessageSchema` shape.
pub fn create_plan_approval_response_message(
    request_id: &str,
    approved: bool,
    feedback: Option<&str>,
    permission_mode: Option<&str>,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "type".to_string(),
        Value::String("plan_approval_response".to_string()),
    );
    object.insert(
        "requestId".to_string(),
        Value::String(request_id.to_string()),
    );
    object.insert("approved".to_string(), Value::Bool(approved));
    // CC key order differs per branch: rejection is {type, requestId,
    // approved, feedback, timestamp} (SendMessageTool.ts:493-499) while
    // approval is {type, requestId, approved, timestamp, permissionMode}
    // (:451-457) — feedback before timestamp, permissionMode after.
    if let Some(feedback) = feedback {
        object.insert("feedback".to_string(), Value::String(feedback.to_string()));
    }
    object.insert(
        "timestamp".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    if let Some(permission_mode) = permission_mode {
        object.insert(
            "permissionMode".to_string(),
            Value::String(permission_mode.to_string()),
        );
    }
    Value::Object(object)
}

/// Maps to: CC `teammateMailbox.ts#isPlanApprovalResponse`.
pub fn is_plan_approval_response(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    let valid = parsed.get("type").and_then(Value::as_str) == Some("plan_approval_response")
        && parsed.get("requestId").and_then(Value::as_str).is_some()
        && parsed.get("approved").and_then(Value::as_bool).is_some()
        && parsed.get("timestamp").and_then(Value::as_str).is_some()
        && parsed
            .get("permissionMode")
            .and_then(Value::as_str)
            .map(crate::utils::permissions::permission_mode::external_permission_mode_from_string)
            .unwrap_or(Some(crate::types::permissions::PermissionMode::Default))
            .is_some();
    valid.then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isTaskAssignment`.
pub fn is_task_assignment(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("task_assignment")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#isTeamPermissionUpdate`.
pub fn is_team_permission_update(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    (parsed.get("type").and_then(Value::as_str) == Some("team_permission_update")).then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#createModeSetRequestMessage`.
pub fn create_mode_set_request_message(mode: &str, from: &str) -> Value {
    serde_json::json!({
        "type": "mode_set_request",
        "mode": mode,
        "from": from,
    })
}

/// Maps to: CC `teammateMailbox.ts#isModeSetRequest`.
pub fn is_mode_set_request(message_text: &str) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(message_text).ok()?;
    let Some(mode) = parsed.get("mode").and_then(Value::as_str) else {
        return None;
    };
    let valid = parsed.get("type").and_then(Value::as_str) == Some("mode_set_request")
        && parsed.get("from").and_then(Value::as_str).is_some()
        && crate::utils::permissions::permission_mode::external_permission_mode_from_string(mode)
            .is_some();
    valid.then_some(parsed)
}

/// Maps to: CC `teammateMailbox.ts#sendShutdownRequestToMailbox`.
pub fn send_shutdown_request_to_mailbox(
    target_name: &str,
    team_name: Option<&str>,
    reason: Option<&str>,
) -> Result<(String, String), String> {
    let resolved_team_name = team_name
        .map(ToOwned::to_owned)
        .or_else(|| crate::utils::teammate::get_team_name(None));
    let sender_name = crate::utils::teammate::get_agent_name()
        .unwrap_or_else(|| crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string());
    let request_id = crate::utils::agent_id::generate_request_id("shutdown", target_name);
    let shutdown_message = create_shutdown_request_message(&request_id, &sender_name, reason);
    write_to_mailbox(
        target_name,
        TeammateMessageInput {
            from: sender_name,
            text: shutdown_message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            color: crate::utils::teammate::get_teammate_color(),
            summary: None,
        },
        resolved_team_name.as_deref(),
    )?;
    Ok((request_id, target_name.to_string()))
}

/// Maps to: CC `teammateMailbox.ts#isStructuredProtocolMessage`.
pub fn is_structured_protocol_message(message_text: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(message_text) else {
        return false;
    };
    matches!(
        parsed.get("type").and_then(Value::as_str),
        Some("permission_request")
            | Some("permission_response")
            | Some("sandbox_permission_request")
            | Some("sandbox_permission_response")
            | Some("shutdown_request")
            | Some("shutdown_approved")
            | Some("team_permission_update")
            | Some("mode_set_request")
            | Some("plan_approval_request")
            | Some("plan_approval_response")
    )
}

/// Maps to: CC `teammateMailbox.ts#markMessagesAsReadByPredicate`.
pub fn mark_messages_as_read_by_predicate(
    agent_name: &str,
    team_name: Option<&str>,
    predicate: impl Fn(&TeammateMessage) -> bool,
) -> bool {
    let team = default_team_name(team_name);
    let mut changed = false;
    {
        let mut mailboxes = MAILBOXES.lock().unwrap();
        if let Some(messages) = mailboxes.get_mut(&(team.clone(), agent_name.to_string())) {
            for message in messages {
                if !message.read && predicate(message) {
                    message.read = true;
                    changed = true;
                }
            }
        }
    }
    if maybe_skip_disk_io_in_tests() {
        return changed;
    }
    let path = inbox_path_buf(agent_name, Some(&team));
    let Ok(_lock) = acquire_mailbox_lock(&path) else {
        return changed;
    };
    let mut messages = read_mailbox_from_disk(agent_name, Some(&team)).unwrap_or_default();
    for message in &mut messages {
        if !message.read && predicate(message) {
            message.read = true;
            changed = true;
        }
    }
    let _ = write_mailbox_to_disk(agent_name, Some(&team), &messages);
    changed
}

#[cfg(test)]
pub static TEST_TEAMMATE_MAILBOX_LOCK: std::sync::LazyLock<TestStateLock> =
    std::sync::LazyLock::new(TestStateLock::new);

#[cfg(test)]
pub fn clear_mailboxes_for_test() {
    MAILBOXES.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn unique_config_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cometix-{prefix}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn write_read_and_mark_mailbox_messages_like_official_inbox_semantics() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        clear_mailboxes_for_test();
        write_to_mailbox(
            "reviewer",
            TeammateMessageInput {
                from: "team-lead".to_string(),
                text: "please review".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                color: Some("blue".to_string()),
                summary: Some("review request".to_string()),
            },
            Some("alpha"),
        )
        .unwrap();
        let messages = read_mailbox("reviewer", Some("alpha"));
        assert_eq!(messages.len(), 1);
        assert!(!messages[0].read);
        assert_eq!(read_unread_messages("reviewer", Some("alpha")).len(), 1);
        assert!(mark_message_as_read_by_index("reviewer", Some("alpha"), 0));
        assert!(read_unread_messages("reviewer", Some("alpha")).is_empty());
        assert!(get_inbox_path("reviewer", Some("Alpha Team")).contains("Alpha-Team"));
    }

    #[test]
    fn file_backed_mailbox_matches_official_inbox_lifecycle() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_mailboxes_for_test();
        let root = unique_config_dir("mailbox");
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        write_to_mailbox(
            "Reviewer One",
            TeammateMessageInput {
                from: "team-lead".to_string(),
                text: "please review".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                color: Some("blue".to_string()),
                summary: Some("review request".to_string()),
            },
            Some("Alpha Team"),
        )
        .unwrap();
        clear_mailboxes_for_test();

        let path = root
            .join("teams")
            .join("Alpha-Team")
            .join("inboxes")
            .join("Reviewer-One.json");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value[0]["from"], "team-lead");
        assert_eq!(value[0]["read"], false);

        let messages = read_mailbox("Reviewer One", Some("Alpha Team"));
        assert_eq!(messages.len(), 1);
        assert!(mark_message_as_read_by_index(
            "Reviewer One",
            Some("Alpha Team"),
            0
        ));
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value[0]["read"], true);
        clear_mailbox("Reviewer One", Some("Alpha Team"));
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn idle_notification_and_peer_dm_summary_match_official_shapes() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let idle = create_idle_notification(
            "reviewer",
            Some("available"),
            Some("[to tester] check logs"),
            None,
            None,
            None,
        );
        let parsed = is_idle_notification(&idle.to_string()).unwrap();
        assert_eq!(parsed.get("from").and_then(Value::as_str), Some("reviewer"));
        assert_eq!(
            parsed.get("idleReason").and_then(Value::as_str),
            Some("available")
        );

        let messages = vec![crate::types::message::Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::ToolUse(
                    crate::types::message::ToolUseBlock {
                        id: crate::types::ids::ToolUseId("toolu_send".to_string()),
                        name: crate::tools::send_message_tool::prompt::SEND_MESSAGE_TOOL_NAME
                            .to_string(),
                        input: serde_json::json!({
                            "to": "tester",
                            "message": "please check the logs and report back",
                            "summary": "check logs",
                        }),
                    },
                )],
                model: None,
                stop_reason: None,
                usage: None,
            },
        )];
        assert_eq!(
            get_last_peer_dm_summary(&messages).as_deref(),
            Some("[to tester] check logs")
        );
    }

    #[test]
    fn permission_and_sandbox_messages_round_trip_json_type_checks() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let request = create_permission_request_message(
            "perm-1",
            "worker",
            "Bash",
            "toolu_1",
            "run ls",
            serde_json::json!({"command":"ls"}),
            vec![serde_json::json!({"rule":"Bash(ls)"})],
        );
        let request_text = request.to_string();
        assert_eq!(
            is_permission_request(&request_text)
                .unwrap()
                .get("tool_name")
                .and_then(Value::as_str),
            Some("Bash")
        );

        let response = create_permission_response_message(
            "perm-1",
            "success",
            None,
            Some(serde_json::json!({"command":"ls -la"})),
            None,
        );
        assert!(is_permission_response(&response.to_string()).is_some());
        let sandbox = create_sandbox_permission_request_message(
            "sandbox-1",
            "worker@team",
            "worker",
            Some("green"),
            "example.com",
        );
        assert!(is_sandbox_permission_request(&sandbox.to_string()).is_some());
        let sandbox_response =
            create_sandbox_permission_response_message("sandbox-1", "example.com", true);
        assert!(is_sandbox_permission_response(&sandbox_response.to_string()).is_some());
    }

    #[test]
    fn mailbox_write_failure_is_returned_and_does_not_commit_in_memory_success() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_mailboxes_for_test();
        let root = unique_config_dir("mailbox-invalid-root");
        std::fs::write(&root, "not a directory").unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let error = write_to_mailbox(
            "reviewer",
            TeammateMessageInput {
                from: "team-lead".to_string(),
                text: "must not be reported as delivered".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                color: None,
                summary: None,
            },
            Some("alpha"),
        )
        .expect_err("invalid config root must propagate mailbox failure");

        assert!(error.contains("Failed to create inbox directory"));
        assert!(MAILBOXES.lock().unwrap().is_empty());
        let _ = std::fs::remove_file(root);
    }

    #[test]
    fn plan_mode_and_team_protocol_messages_match_official_mailbox_shapes() {
        let _lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let plan_request = create_plan_approval_request_message(
            "planner",
            "plan-1",
            "/tmp/plan.md",
            "## Plan\n\nShip it",
        );
        assert_eq!(
            is_plan_approval_request(&plan_request.to_string())
                .unwrap()
                .get("planFilePath")
                .and_then(Value::as_str),
            Some("/tmp/plan.md")
        );

        let plan_response = create_plan_approval_response_message(
            "plan-1",
            true,
            Some("looks good"),
            Some("acceptEdits"),
        );
        assert_eq!(
            is_plan_approval_response(&plan_response.to_string())
                .unwrap()
                .get("permissionMode")
                .and_then(Value::as_str),
            Some("acceptEdits")
        );
        let invalid_plan_response = serde_json::json!({
            "type": "plan_approval_response",
            "requestId": "plan-1",
            "approved": true,
            "timestamp": "now",
            "permissionMode": "not-a-mode",
        });
        assert!(is_plan_approval_response(&invalid_plan_response.to_string()).is_none());

        let mode_set = create_mode_set_request_message("dontAsk", "team-lead");
        assert!(is_mode_set_request(&mode_set.to_string()).is_some());
        let invalid_mode = create_mode_set_request_message("bubble", "team-lead");
        assert!(is_mode_set_request(&invalid_mode.to_string()).is_none());

        let team_update = serde_json::json!({
            "type": "team_permission_update",
            "permissionUpdate": {
                "type": "addRules",
                "rules": [{"toolName": "Edit", "ruleContent": "/tmp"}],
                "behavior": "allow",
                "destination": "session",
            },
            "directoryPath": "/tmp",
            "toolName": "Edit",
        });
        assert!(is_team_permission_update(&team_update.to_string()).is_some());
        let task_assignment = serde_json::json!({
            "type": "task_assignment",
            "taskId": "task-1",
            "subject": "Review",
            "description": "Review patch",
            "assignedBy": "team-lead",
            "timestamp": "now",
        });
        assert!(is_task_assignment(&task_assignment.to_string()).is_some());
        assert!(is_structured_protocol_message(&plan_request.to_string()));
    }

    #[test]
    fn send_shutdown_request_to_mailbox_uses_dynamic_teammate_identity_like_official() {
        let _mailbox_lock = TEST_TEAMMATE_MAILBOX_LOCK.lock().unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        clear_mailboxes_for_test();
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

        let (request_id, target) =
            send_shutdown_request_to_mailbox("tester", None, Some("done")).unwrap();

        assert!(request_id.starts_with("shutdown-"));
        assert_eq!(target, "tester");
        let inbox = read_mailbox("tester", Some("alpha"));
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "reviewer");
        assert_eq!(inbox[0].color.as_deref(), Some("green"));
        let parsed = is_shutdown_request(&inbox[0].text).unwrap();
        assert_eq!(parsed.get("from").and_then(Value::as_str), Some("reviewer"));
        assert_eq!(parsed.get("reason").and_then(Value::as_str), Some("done"));

        crate::utils::teammate::clear_dynamic_team_context();
    }
}
