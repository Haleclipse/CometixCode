//! Synchronized permission prompts for agent swarms.
//!
//! Maps to: CC `utils/swarm/permissionSync.ts`.
//!
//! Official CC stores pending/resolved permission requests on disk and also
//! routes permission requests/responses through teammate mailboxes. Cometix keeps
//! the official request/response shapes and function boundaries in memory until
//! full team-file persistence is enabled.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Maps to: CC `permissionSync.ts#SwarmPermissionRequest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmPermissionRequest {
    pub id: String,
    pub worker_id: String,
    pub worker_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_color: Option<String>,
    pub team_name: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub description: String,
    pub input: Value,
    pub permission_suggestions: Vec<Value>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_updates: Option<Vec<Value>>,
    pub created_at: u64,
}

/// Maps to: CC `permissionSync.ts#PermissionResolution`.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionResolution {
    pub decision: String,
    pub resolved_by: String,
    pub feedback: Option<String>,
    pub updated_input: Option<Value>,
    pub permission_updates: Option<Vec<Value>>,
}

/// Maps to: CC `permissionSync.ts#PermissionResponse`.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionResponse {
    pub request_id: String,
    pub decision: String,
    pub timestamp: String,
    pub feedback: Option<String>,
    pub updated_input: Option<Value>,
    pub permission_updates: Option<Vec<Value>>,
}

/// Maps to: CC `permissionSync.ts#createPermissionRequest` params.
#[derive(Clone, Debug, PartialEq)]
pub struct CreatePermissionRequestParams {
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: Value,
    pub description: String,
    pub permission_suggestions: Vec<Value>,
    pub team_name: Option<String>,
    pub worker_id: Option<String>,
    pub worker_name: Option<String>,
    pub worker_color: Option<String>,
}

#[derive(Default)]
struct PermissionStore {
    pending: HashMap<String, Vec<SwarmPermissionRequest>>,
    resolved: HashMap<String, Vec<SwarmPermissionRequest>>,
}

static PERMISSION_STORE: LazyLock<Mutex<PermissionStore>> =
    LazyLock::new(|| Mutex::new(PermissionStore::default()));

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// Maps to: CC `permissionSync.ts#getPermissionDir`.
pub fn get_permission_dir(team_name: &str) -> String {
    format!(
        "~/.claude/teams/{}/permissions",
        crate::utils::swarm::team_helpers::sanitize_name(team_name)
    )
}

/// Maps to: CC `permissionSync.ts#generateRequestId`.
pub fn generate_request_id() -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("perm-{}-{}", now_ms(), &random[..7])
}

/// Maps to: CC `permissionSync.ts#generateSandboxRequestId`.
pub fn generate_sandbox_request_id() -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("sandbox-{}-{}", now_ms(), &random[..7])
}

/// Maps to: CC `permissionSync.ts#isTeamLeader`.
pub fn is_team_leader() -> bool {
    let agent_id = crate::utils::teammate::get_agent_id();
    agent_id
        .as_deref()
        .is_none_or(|id| id.is_empty() || id == "team-lead")
}

/// Maps to: CC `permissionSync.ts#isSwarmWorker`.
pub fn is_swarm_worker() -> bool {
    let team_name = crate::utils::teammate::get_team_name(None);
    let agent_id = crate::utils::teammate::get_agent_id();
    team_name.is_some_and(|name| !name.is_empty())
        && agent_id.is_some_and(|id| !id.is_empty())
        && !is_team_leader()
}

/// Maps to: CC `permissionSync.ts#createPermissionRequest`.
pub fn create_permission_request(
    params: CreatePermissionRequestParams,
) -> Result<SwarmPermissionRequest, String> {
    let team_name = params
        .team_name
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
        .ok_or_else(|| "Team name is required for permission requests".to_string())?;
    let worker_id = params
        .worker_id
        .ok_or_else(|| "Worker ID is required for permission requests".to_string())?;
    let worker_name = params
        .worker_name
        .ok_or_else(|| "Worker name is required for permission requests".to_string())?;

    Ok(SwarmPermissionRequest {
        id: generate_request_id(),
        worker_id,
        worker_name,
        worker_color: params.worker_color,
        team_name,
        tool_name: params.tool_name,
        tool_use_id: params.tool_use_id,
        description: params.description,
        input: params.input,
        permission_suggestions: params.permission_suggestions,
        status: "pending".to_string(),
        resolved_by: None,
        resolved_at: None,
        feedback: None,
        updated_input: None,
        permission_updates: None,
        created_at: now_ms(),
    })
}

/// Maps to: CC `permissionSync.ts#writePermissionRequest`.
pub fn write_permission_request(request: SwarmPermissionRequest) -> SwarmPermissionRequest {
    PERMISSION_STORE
        .lock()
        .unwrap()
        .pending
        .entry(request.team_name.clone())
        .or_default()
        .push(request.clone());
    request
}

/// Maps to: CC `permissionSync.ts#readPendingPermissions`.
pub fn read_pending_permissions(team_name: Option<&str>) -> Vec<SwarmPermissionRequest> {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return Vec::new();
    };
    let mut requests = PERMISSION_STORE
        .lock()
        .unwrap()
        .pending
        .get(&team)
        .cloned()
        .unwrap_or_default();
    requests.sort_by_key(|request| request.created_at);
    requests
}

/// Maps to: CC `permissionSync.ts#readResolvedPermission`.
pub fn read_resolved_permission(
    request_id: &str,
    team_name: Option<&str>,
) -> Option<SwarmPermissionRequest> {
    let team = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)?;
    PERMISSION_STORE
        .lock()
        .unwrap()
        .resolved
        .get(&team)?
        .iter()
        .find(|request| request.id == request_id)
        .cloned()
}

/// Maps to: CC `permissionSync.ts#resolvePermission`.
pub fn resolve_permission(
    request_id: &str,
    resolution: PermissionResolution,
    team_name: Option<&str>,
) -> bool {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return false;
    };
    let mut store = PERMISSION_STORE.lock().unwrap();
    let Some(pending) = store.pending.get_mut(&team) else {
        return false;
    };
    let Some(index) = pending.iter().position(|request| request.id == request_id) else {
        return false;
    };
    let mut request = pending.remove(index);
    request.status = if resolution.decision == "approved" {
        "approved".to_string()
    } else {
        "rejected".to_string()
    };
    request.resolved_by = Some(resolution.resolved_by);
    request.resolved_at = Some(now_ms());
    request.feedback = resolution.feedback;
    request.updated_input = resolution.updated_input;
    request.permission_updates = resolution.permission_updates;
    store.resolved.entry(team).or_default().push(request);
    true
}

/// Maps to: CC `permissionSync.ts#cleanupOldResolutions`.
pub fn cleanup_old_resolutions(team_name: Option<&str>, max_age_ms: u64) -> usize {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return 0;
    };
    let now = now_ms();
    let mut store = PERMISSION_STORE.lock().unwrap();
    let Some(resolved) = store.resolved.get_mut(&team) else {
        return 0;
    };
    let original = resolved.len();
    resolved.retain(|request| {
        let timestamp = request.resolved_at.unwrap_or(request.created_at);
        now.saturating_sub(timestamp) < max_age_ms
    });
    original - resolved.len()
}

/// Maps to: CC `permissionSync.ts#pollForResponse`.
pub fn poll_for_response(request_id: &str, team_name: Option<&str>) -> Option<PermissionResponse> {
    let resolved = read_resolved_permission(request_id, team_name)?;
    Some(PermissionResponse {
        request_id: resolved.id,
        decision: if resolved.status == "approved" {
            "approved".to_string()
        } else {
            "denied".to_string()
        },
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            resolved.resolved_at.unwrap_or(resolved.created_at) as i64,
        )
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339(),
        feedback: resolved.feedback,
        updated_input: resolved.updated_input,
        permission_updates: resolved.permission_updates,
    })
}

/// Maps to: CC `permissionSync.ts#deleteResolvedPermission`.
pub fn delete_resolved_permission(request_id: &str, team_name: Option<&str>) -> bool {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return false;
    };
    let mut store = PERMISSION_STORE.lock().unwrap();
    let Some(resolved) = store.resolved.get_mut(&team) else {
        return false;
    };
    let original = resolved.len();
    resolved.retain(|request| request.id != request_id);
    resolved.len() != original
}

/// Maps to: CC `permissionSync.ts#removeWorkerResponse`.
pub fn remove_worker_response(request_id: &str, team_name: Option<&str>) {
    let _ = delete_resolved_permission(request_id, team_name);
}

/// Maps to: CC `permissionSync.ts:651-667` `getLeaderName` — "Get the leader's
/// name from the team file" via `readTeamFileAsync` (`:657`), a pure disk read.
/// This runs on the teammate side (a teammate asking where to post a permission
/// request), where `TEAM_TOOL_STATE` is the wrong process's snapshot entirely.
pub fn get_leader_name(team_name: Option<&str>) -> Option<String> {
    let team = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)?;
    let record = crate::utils::swarm::team_helpers::read_team_file(&team)?;
    record
        .members
        .iter()
        .find(|member| member.agent_id == record.lead_agent_id)
        .map(|member| member.name.clone())
        .or_else(|| Some(crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()))
}

/// Maps to: CC `permissionSync.ts#sendPermissionRequestViaMailbox`.
pub fn send_permission_request_via_mailbox(request: &SwarmPermissionRequest) -> bool {
    let Some(leader_name) = get_leader_name(Some(&request.team_name)) else {
        return false;
    };
    let message = crate::utils::teammate_mailbox::create_permission_request_message(
        &request.id,
        &request.worker_name,
        &request.tool_name,
        &request.tool_use_id,
        &request.description,
        request.input.clone(),
        request.permission_suggestions.clone(),
    );
    crate::utils::teammate_mailbox::write_to_mailbox(
        &leader_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: request.worker_name.clone(),
            text: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: request.worker_color.clone(),
            summary: None,
        },
        Some(&request.team_name),
    )
    .is_ok()
}

/// Maps to: CC `permissionSync.ts#sendSandboxPermissionRequestViaMailbox`.
pub fn send_sandbox_permission_request_via_mailbox(
    host: &str,
    request_id: &str,
    team_name: Option<&str>,
) -> bool {
    let team = team_name
        .map(str::to_string)
        .or_else(|| crate::utils::teammate::get_team_name(None));
    let Some(team) = team.filter(|name| !name.is_empty()) else {
        return false;
    };
    let Some(leader_name) = get_leader_name(Some(&team)) else {
        return false;
    };
    let Some(worker_id) = crate::utils::teammate::get_agent_id().filter(|id| !id.is_empty()) else {
        return false;
    };
    let Some(worker_name) =
        crate::utils::teammate::get_agent_name().filter(|name| !name.is_empty())
    else {
        return false;
    };
    let worker_color = crate::utils::teammate::get_teammate_color();
    let message = crate::utils::teammate_mailbox::create_sandbox_permission_request_message(
        request_id,
        &worker_id,
        &worker_name,
        worker_color.as_deref(),
        host,
    );
    crate::utils::teammate_mailbox::write_to_mailbox(
        &leader_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: worker_name,
            text: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: worker_color,
            summary: None,
        },
        Some(&team),
    )
    .is_ok()
}

/// Maps to: CC `permissionSync.ts#sendSandboxPermissionResponseViaMailbox`.
pub fn send_sandbox_permission_response_via_mailbox(
    worker_name: &str,
    request_id: &str,
    host: &str,
    allow: bool,
    team_name: Option<&str>,
) -> bool {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return false;
    };
    let message = crate::utils::teammate_mailbox::create_sandbox_permission_response_message(
        request_id, host, allow,
    );
    crate::utils::teammate_mailbox::write_to_mailbox(
        worker_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: crate::utils::teammate::get_agent_name()
                .unwrap_or_else(|| crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()),
            text: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: None,
            summary: None,
        },
        Some(&team),
    )
    .is_ok()
}

/// Maps to: CC `permissionSync.ts#sendPermissionResponseViaMailbox`.
pub fn send_permission_response_via_mailbox(
    worker_name: &str,
    resolution: &PermissionResolution,
    request_id: &str,
    team_name: Option<&str>,
) -> bool {
    let Some(team) = team_name
        .map(ToOwned::to_owned)
        .or_else(crate::utils::swarm::team_helpers::current_team_name)
    else {
        return false;
    };
    let message = crate::utils::teammate_mailbox::create_permission_response_message(
        request_id,
        if resolution.decision == "approved" {
            "success"
        } else {
            "error"
        },
        resolution.feedback.as_deref(),
        resolution.updated_input.clone(),
        resolution.permission_updates.clone(),
    );
    crate::utils::teammate_mailbox::write_to_mailbox(
        worker_name,
        crate::utils::teammate_mailbox::TeammateMessageInput {
            from: crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string(),
            text: message.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            color: None,
            summary: None,
        },
        Some(&team),
    )
    .is_ok()
}

#[cfg(test)]
pub static TEST_PERMISSION_SYNC_LOCK: std::sync::LazyLock<TestStateLock> =
    std::sync::LazyLock::new(TestStateLock::new);

#[cfg(test)]
pub fn clear_permission_sync_for_test() {
    *PERMISSION_STORE.lock().unwrap() = PermissionStore::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// `get_leader_name` reads the team file from disk (CC
    /// `permissionSync.ts:657` `readTeamFileAsync`), so a memory-only seed is a
    /// no-op fixture — `write_team_record` suppresses the write unless
    /// `COMETIX_TEST_TEAM_FILE_IO` is on. Pin a scratch config root and enable
    /// the team-file and mailbox disk paths; the returned guards must outlive
    /// the test body.
    fn seed_team() -> (EnvVarGuard, EnvVarGuard, EnvVarGuard) {
        let root = std::env::temp_dir().join(format!(
            "cometix-permission-sync-{}",
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

    #[test]
    fn request_lifecycle_moves_pending_to_resolved_and_polls_response() {
        let _permission_lock = TEST_PERMISSION_SYNC_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_permission_sync_for_test();
        let _team_guards = seed_team();
        let request = create_permission_request(CreatePermissionRequestParams {
            tool_name: "Bash".to_string(),
            tool_use_id: "toolu_1".to_string(),
            input: serde_json::json!({"command":"ls"}),
            description: "run ls".to_string(),
            permission_suggestions: Vec::new(),
            team_name: Some("alpha".to_string()),
            worker_id: Some("worker@alpha".to_string()),
            worker_name: Some("worker".to_string()),
            worker_color: Some("green".to_string()),
        })
        .unwrap();
        let request_id = request.id.clone();
        write_permission_request(request);
        assert_eq!(read_pending_permissions(Some("alpha")).len(), 1);
        assert!(resolve_permission(
            &request_id,
            PermissionResolution {
                decision: "approved".to_string(),
                resolved_by: "leader".to_string(),
                feedback: None,
                updated_input: Some(serde_json::json!({"command":"ls -la"})),
                permission_updates: Some(vec![serde_json::json!({"rule":"Bash(ls)"})]),
            },
            Some("alpha"),
        ));
        assert!(read_pending_permissions(Some("alpha")).is_empty());
        let response = poll_for_response(&request_id, Some("alpha")).unwrap();
        assert_eq!(response.decision, "approved");
        assert_eq!(
            response.updated_input.as_ref().unwrap()["command"].as_str(),
            Some("ls -la")
        );
        assert!(delete_resolved_permission(&request_id, Some("alpha")));
        assert!(poll_for_response(&request_id, Some("alpha")).is_none());
    }

    #[test]
    fn sandbox_permission_response_uses_official_mailbox_protocol() {
        let _permission_lock = TEST_PERMISSION_SYNC_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _team_guards = seed_team();

        assert!(send_sandbox_permission_response_via_mailbox(
            "worker",
            "sandbox-1",
            "example.com",
            true,
            Some("alpha"),
        ));
        let messages =
            crate::utils::teammate_mailbox::read_unread_messages("worker", Some("alpha"));
        assert_eq!(messages.len(), 1);
        let parsed =
            crate::utils::teammate_mailbox::is_sandbox_permission_response(&messages[0].text)
                .expect("sandbox response");
        assert_eq!(
            parsed.get("requestId").and_then(Value::as_str),
            Some("sandbox-1")
        );
        assert_eq!(
            parsed.get("host").and_then(Value::as_str),
            Some("example.com")
        );
        assert_eq!(parsed.get("allow").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn mailbox_helpers_route_permission_request_and_response() {
        let _permission_lock = TEST_PERMISSION_SYNC_LOCK.lock().unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _team_guards = seed_team();
        let request = create_permission_request(CreatePermissionRequestParams {
            tool_name: "Edit".to_string(),
            tool_use_id: "toolu_2".to_string(),
            input: serde_json::json!({"file_path":"/tmp/a"}),
            description: "edit file".to_string(),
            permission_suggestions: Vec::new(),
            team_name: Some("alpha".to_string()),
            worker_id: Some("worker@alpha".to_string()),
            worker_name: Some("worker".to_string()),
            worker_color: None,
        })
        .unwrap();
        assert!(send_permission_request_via_mailbox(&request));
        let leader_messages = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        assert_eq!(leader_messages.len(), 1);
        assert!(
            crate::utils::teammate_mailbox::is_permission_request(&leader_messages[0].text)
                .is_some()
        );
        assert!(send_permission_response_via_mailbox(
            "worker",
            &PermissionResolution {
                decision: "rejected".to_string(),
                resolved_by: "leader".to_string(),
                feedback: Some("no".to_string()),
                updated_input: None,
                permission_updates: None,
            },
            &request.id,
            Some("alpha"),
        ));
        let worker_messages = crate::utils::teammate_mailbox::read_mailbox("worker", Some("alpha"));
        assert_eq!(worker_messages.len(), 1);
        assert!(
            crate::utils::teammate_mailbox::is_permission_response(&worker_messages[0].text)
                .is_some()
        );
    }

    #[test]
    fn is_swarm_worker_matches_official_team_and_agent_gate() {
        let _guard = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        crate::utils::teammate::set_dynamic_team_context(None);
        assert!(!is_swarm_worker());
        assert!(is_team_leader());

        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "worker-1".to_string(),
                agent_name: "worker".to_string(),
                team_name: "alpha".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        assert!(is_swarm_worker());
        assert!(!is_team_leader());

        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "team-lead".to_string(),
                agent_name: "lead".to_string(),
                team_name: "alpha".to_string(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        assert!(!is_swarm_worker());
        assert!(is_team_leader());
        crate::utils::teammate::set_dynamic_team_context(None);
    }

    #[test]
    fn sandbox_permission_request_uses_official_mailbox_protocol() {
        let _permission_lock = TEST_PERMISSION_SYNC_LOCK.lock().unwrap();
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
        clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _team_guards = seed_team();
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "worker-1".to_string(),
                agent_name: "worker".to_string(),
                team_name: "alpha".to_string(),
                color: Some("blue".to_string()),
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));

        assert!(send_sandbox_permission_request_via_mailbox(
            "api.example.com",
            "sandbox-req-1",
            Some("alpha"),
        ));
        let leader_messages = crate::utils::teammate_mailbox::read_mailbox(
            crate::utils::swarm::constants::TEAM_LEAD_NAME,
            Some("alpha"),
        );
        assert_eq!(leader_messages.len(), 1);
        let parsed =
            crate::utils::teammate_mailbox::is_sandbox_permission_request(&leader_messages[0].text)
                .expect("sandbox request");
        assert_eq!(
            parsed.get("requestId").and_then(|v| v.as_str()),
            Some("sandbox-req-1")
        );
        assert_eq!(
            parsed
                .get("hostPattern")
                .and_then(|v| v.get("host"))
                .and_then(|v| v.as_str()),
            Some("api.example.com")
        );
        crate::utils::teammate::set_dynamic_team_context(None);
    }
}
