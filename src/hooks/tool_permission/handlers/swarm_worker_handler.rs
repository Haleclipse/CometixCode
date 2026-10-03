//! Maps to: CC `hooks/toolPermission/handlers/swarmWorkerHandler.ts`.
//!
//! Official swarm-worker handling gates on swarm mode, tries classifier
//! auto-approval, then forwards a permission request to the leader mailbox and
//! waits for a registered callback. This Rust slice keeps the official state
//! transitions and request shape without performing mailbox side effects; the
//! full async callback/poller runtime remains in the swarm permission-sync
//! layer. AppState `pendingWorkerRequest` writes match CC `setAppState`.

use super::coordinator_handler::AutomatedPermissionCheck;
use crate::hooks::use_inbox_poller::PendingWorkerRequest;
use crate::state::store::AppStore;
use crate::types::permissions::{PermissionUpdate, PromptDecision};
use crate::utils::swarm::permission_sync::{
    CreatePermissionRequestParams, SwarmPermissionRequest, create_permission_request,
};

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmWorkerPermissionParams {
    pub agent_swarms_enabled: bool,
    pub is_swarm_worker: bool,
    pub description: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: serde_json::Value,
    pub pending_classifier_check: bool,
    pub updated_input: Option<serde_json::Value>,
    pub suggestions: Vec<PermissionUpdate>,
    pub classifier_result: AutomatedPermissionCheck,
    pub bash_classifier_enabled: bool,
    pub team_name: Option<String>,
    pub worker_id: Option<String>,
    pub worker_name: Option<String>,
    pub worker_color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SwarmWorkerPermissionOutcome {
    Fallthrough,
    Decision(PromptDecision),
    ForwardedToLeader {
        request: SwarmPermissionRequest,
        pending_tool_name: String,
        pending_tool_use_id: String,
        pending_description: String,
        mailbox_sent: bool,
    },
    ForwardFailed {
        error: String,
    },
}

fn permission_updates_to_values(updates: &[PermissionUpdate]) -> Vec<serde_json::Value> {
    crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(
        updates,
    )
}

/// Maps to: CC `handleSwarmWorkerPermission(...)`.
pub fn handle_swarm_worker_permission(
    params: SwarmWorkerPermissionParams,
) -> SwarmWorkerPermissionOutcome {
    if !params.agent_swarms_enabled || !params.is_swarm_worker {
        return SwarmWorkerPermissionOutcome::Fallthrough;
    }

    if params.bash_classifier_enabled {
        match params.classifier_result {
            AutomatedPermissionCheck::Resolved(decision) => {
                return SwarmWorkerPermissionOutcome::Decision(decision);
            }
            AutomatedPermissionCheck::Failed(error) => {
                return SwarmWorkerPermissionOutcome::ForwardFailed { error };
            }
            AutomatedPermissionCheck::Unresolved => {}
        }
    }

    let _ = (params.pending_classifier_check, params.updated_input);
    let request = match create_permission_request(CreatePermissionRequestParams {
        tool_name: params.tool_name.clone(),
        tool_use_id: params.tool_use_id.clone(),
        input: params.input.clone(),
        description: params.description.clone(),
        permission_suggestions: permission_updates_to_values(&params.suggestions),
        team_name: params.team_name,
        worker_id: params.worker_id,
        worker_name: params.worker_name,
        worker_color: params.worker_color,
    }) {
        Ok(request) => request,
        Err(error) => return SwarmWorkerPermissionOutcome::ForwardFailed { error },
    };

    SwarmWorkerPermissionOutcome::ForwardedToLeader {
        request,
        pending_tool_name: params.tool_name,
        pending_tool_use_id: params.tool_use_id,
        pending_description: params.description,
        mailbox_sent: false,
    }
}

/// Maps to: CC `setAppState` → `pendingWorkerRequest: { toolName, toolUseId, description }`.
/// P4 identity: fresh `Arc::new` per write mirrors CC's whole-object write
/// (swarmWorkerHandler.ts:62-65/:126-133).
pub fn set_pending_worker_request(store: &AppStore, pending: Option<PendingWorkerRequest>) {
    store.replace_with(|state| {
        state.pending_worker_request = pending.map(std::sync::Arc::new);
    });
}

/// Maps to: CC `clearPendingRequest` → `pendingWorkerRequest: null`.
pub fn clear_pending_worker_request(store: &AppStore) {
    set_pending_worker_request(store, None);
}

/// Apply the pending indicator after a successful forward-to-leader outcome
/// (CC body after `sendPermissionRequestViaMailbox`).
pub fn apply_pending_worker_request_from_outcome(
    store: &AppStore,
    outcome: &SwarmWorkerPermissionOutcome,
) {
    if let SwarmWorkerPermissionOutcome::ForwardedToLeader {
        pending_tool_name,
        pending_tool_use_id,
        pending_description,
        ..
    } = outcome
    {
        set_pending_worker_request(
            store,
            Some(PendingWorkerRequest {
                tool_name: pending_tool_name.clone(),
                tool_use_id: pending_tool_use_id.clone(),
                description: pending_description.clone(),
            }),
        );
    }
}

/// Maps to: CC `handleSwarmWorkerPermission` send + `setAppState(pendingWorkerRequest)`
/// + `registerPermissionCallback` wait (Cometix: sync until timeout/abort).
///
/// Returns `Some(decision)` when swarm handling resolved the ask; `None` means
/// fall through to local interactive handling (CC `return null`).
pub fn try_resolve_swarm_worker_ask(
    tool_name: &str,
    tool_use_id: &str,
    description: &str,
    input: &serde_json::Value,
    suggestions: &[PermissionUpdate],
    app_store: Option<&AppStore>,
    wait_timeout: std::time::Duration,
    mut is_aborted: impl FnMut() -> bool,
) -> Option<PromptDecision> {
    if !crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        || !crate::utils::swarm::permission_sync::is_swarm_worker()
    {
        return None;
    }

    let team_name = crate::utils::teammate::get_team_name(None);
    let worker_id = crate::utils::teammate::get_agent_id();
    let worker_name = crate::utils::teammate::get_agent_name();
    let worker_color = crate::utils::teammate::get_teammate_color();

    let outcome = handle_swarm_worker_permission(SwarmWorkerPermissionParams {
        agent_swarms_enabled: true,
        is_swarm_worker: true,
        description: description.to_string(),
        tool_name: tool_name.to_string(),
        tool_use_id: tool_use_id.to_string(),
        input: input.clone(),
        pending_classifier_check: false,
        updated_input: None,
        suggestions: suggestions.to_vec(),
        classifier_result: AutomatedPermissionCheck::Unresolved,
        bash_classifier_enabled: false,
        team_name: team_name.clone(),
        worker_id,
        worker_name,
        worker_color,
    });

    match outcome {
        SwarmWorkerPermissionOutcome::Fallthrough => None,
        SwarmWorkerPermissionOutcome::ForwardFailed { .. } => None,
        SwarmWorkerPermissionOutcome::Decision(decision) => Some(decision),
        SwarmWorkerPermissionOutcome::ForwardedToLeader {
            request,
            pending_tool_name,
            pending_tool_use_id,
            pending_description,
            ..
        } => {
            let request_id = request.id.clone();
            let team = request.team_name.clone();
            let original_input = input.clone();
            let decision_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<PromptDecision>));
            let allow_slot = std::sync::Arc::clone(&decision_slot);
            let reject_slot = std::sync::Arc::clone(&decision_slot);
            let store_for_allow = app_store.cloned();
            let store_for_reject = app_store.cloned();

            // Register BEFORE send (CC race avoidance).
            crate::hooks::use_swarm_permission_poller::register_permission_callback(
                crate::hooks::use_swarm_permission_poller::PermissionResponseCallback {
                    request_id: request_id.clone(),
                    tool_use_id: tool_use_id.to_string(),
                    on_allow: std::sync::Arc::new(move |allowed_input, updates, feedback| {
                        if let Some(ref store) = store_for_allow {
                            clear_pending_worker_request(store);
                        }
                        let final_input = match allowed_input {
                            Some(value) if value.as_object().is_some_and(|o| !o.is_empty()) => {
                                Some(value)
                            }
                            _ => Some(original_input.clone()),
                        };
                        *allow_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(PromptDecision {
                                behavior: crate::types::permissions::PermissionBehavior::Allow,
                                choice:
                                    crate::types::permissions::PermissionPromptChoice::AllowOnce,
                                updates,
                                transcript: feedback.unwrap_or_default(),
                                updated_input: final_input,
                            });
                    }),
                    on_reject: std::sync::Arc::new(move |feedback| {
                        if let Some(ref store) = store_for_reject {
                            clear_pending_worker_request(store);
                        }
                        *reject_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(PromptDecision {
                                behavior: crate::types::permissions::PermissionBehavior::Deny,
                                choice: crate::types::permissions::PermissionPromptChoice::Deny,
                                updates: Vec::new(),
                                transcript: feedback.unwrap_or_default(),
                                updated_input: None,
                            });
                    }),
                },
            );

            crate::utils::swarm::permission_sync::write_permission_request(request.clone());
            if !crate::utils::swarm::permission_sync::send_permission_request_via_mailbox(&request)
            {
                crate::hooks::use_swarm_permission_poller::unregister_permission_callback(
                    &request_id,
                );
                return None;
            }
            if let Some(store) = app_store {
                set_pending_worker_request(
                    store,
                    Some(PendingWorkerRequest {
                        tool_name: pending_tool_name,
                        tool_use_id: pending_tool_use_id,
                        description: pending_description,
                    }),
                );
            }

            let start = std::time::Instant::now();
            let poll_ms = 50u64;
            let decision = loop {
                if is_aborted() {
                    crate::hooks::use_swarm_permission_poller::unregister_permission_callback(
                        &request_id,
                    );
                    if let Some(store) = app_store {
                        clear_pending_worker_request(store);
                    }
                    break PromptDecision {
                        behavior: crate::types::permissions::PermissionBehavior::Deny,
                        choice: crate::types::permissions::PermissionPromptChoice::Deny,
                        updates: Vec::new(),
                        transcript: "Cancelled while waiting for team lead approval".to_string(),
                        updated_input: None,
                    };
                }
                if let Some(decision) = decision_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                {
                    break decision;
                }
                // File-poll fallback (CC useSwarmPermissionPoller interval).
                if let Some(response) = crate::utils::swarm::permission_sync::poll_for_response(
                    &request_id,
                    Some(&team),
                ) {
                    let _ = crate::utils::swarm::permission_sync::delete_resolved_permission(
                        &request_id,
                        Some(&team),
                    );
                    let _ = crate::hooks::use_swarm_permission_poller::process_permission_response(
                        &response,
                    );
                    if let Some(decision) = decision_slot
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                    {
                        break decision;
                    }
                }
                if start.elapsed() >= wait_timeout {
                    crate::hooks::use_swarm_permission_poller::unregister_permission_callback(
                        &request_id,
                    );
                    if let Some(store) = app_store {
                        clear_pending_worker_request(store);
                    }
                    break PromptDecision {
                        behavior: crate::types::permissions::PermissionBehavior::Deny,
                        choice: crate::types::permissions::PermissionPromptChoice::Deny,
                        updates: Vec::new(),
                        transcript: "Timed out waiting for team lead approval".to_string(),
                        updated_input: None,
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            };

            Some(decision)
        }
    }
}

/// Maps to: CC REPL worker sandbox path setting `pendingSandboxRequest` after
/// `sendSandboxPermissionRequestViaMailbox`, plus
/// `registerSandboxPermissionCallback`.
///
/// Returns a receiver that resolves when the leader's mailbox response is
/// processed (`process_sandbox_permission_response`). `None` means fall through
/// to local sandbox UI (send failed / not a swarm worker).
pub fn begin_pending_sandbox_request(
    store: &AppStore,
    host: &str,
) -> Option<async_channel::Receiver<bool>> {
    if !crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        || !crate::utils::swarm::permission_sync::is_swarm_worker()
    {
        return None;
    }
    let request_id = crate::utils::swarm::permission_sync::generate_sandbox_request_id();
    if !crate::utils::swarm::permission_sync::send_sandbox_permission_request_via_mailbox(
        host,
        &request_id,
        None,
    ) {
        return None;
    }
    let (tx, rx) = async_channel::bounded(1);
    crate::hooks::use_swarm_permission_poller::register_sandbox_permission_callback(
        crate::hooks::use_swarm_permission_poller::SandboxPermissionResponseCallback {
            request_id: request_id.clone(),
            host: host.to_string(),
            resolve: std::sync::Arc::new(move |allow| {
                let _ = tx.try_send(allow);
            }),
        },
    );
    crate::hooks::use_inbox_poller::set_pending_sandbox_request(
        store,
        Some(crate::hooks::use_inbox_poller::PendingSandboxRequest {
            request_id,
            host: host.to_string(),
        }),
    );
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionPromptChoice, PermissionRuleValue,
        PermissionUpdateDestination,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn decision() -> PromptDecision {
        PromptDecision {
            behavior: PermissionBehavior::Allow,
            choice: PermissionPromptChoice::AllowOnce,
            updates: Vec::new(),
            transcript: "classifier".to_string(),
            updated_input: Some(serde_json::json!({"command":"echo ok"})),
        }
    }

    fn params() -> SwarmWorkerPermissionParams {
        SwarmWorkerPermissionParams {
            agent_swarms_enabled: true,
            is_swarm_worker: true,
            description: "Run command?".to_string(),
            tool_name: "Bash".to_string(),
            tool_use_id: "toolu_1".to_string(),
            input: serde_json::json!({"command":"echo hi"}),
            pending_classifier_check: true,
            updated_input: None,
            suggestions: vec![PermissionUpdate::AddRules {
                destination: PermissionUpdateDestination::LocalSettings,
                behavior: PermissionBehavior::Allow,
                rules: vec![PermissionRuleValue::new("Bash", Some("echo:*".to_string()))],
            }],
            classifier_result: AutomatedPermissionCheck::Unresolved,
            bash_classifier_enabled: true,
            team_name: Some("team".to_string()),
            worker_id: Some("agent-2".to_string()),
            worker_name: Some("Worker".to_string()),
            worker_color: Some("blue".to_string()),
        }
    }

    #[test]
    fn swarm_worker_permission_falls_through_when_gate_disabled() {
        assert_eq!(
            handle_swarm_worker_permission(SwarmWorkerPermissionParams {
                agent_swarms_enabled: false,
                ..params()
            }),
            SwarmWorkerPermissionOutcome::Fallthrough
        );
        assert_eq!(
            handle_swarm_worker_permission(SwarmWorkerPermissionParams {
                is_swarm_worker: false,
                ..params()
            }),
            SwarmWorkerPermissionOutcome::Fallthrough
        );
    }

    #[test]
    fn swarm_worker_permission_returns_classifier_decision_before_forwarding() {
        let outcome = handle_swarm_worker_permission(SwarmWorkerPermissionParams {
            classifier_result: AutomatedPermissionCheck::Resolved(decision()),
            ..params()
        });
        assert!(matches!(outcome, SwarmWorkerPermissionOutcome::Decision(_)));
    }

    #[test]
    fn swarm_worker_permission_builds_leader_request_and_pending_indicator() {
        let outcome = handle_swarm_worker_permission(params());
        let SwarmWorkerPermissionOutcome::ForwardedToLeader {
            request,
            pending_tool_name,
            pending_tool_use_id,
            pending_description,
            mailbox_sent,
        } = outcome
        else {
            panic!("expected forwarded outcome");
        };
        assert_eq!(request.team_name, "team");
        assert_eq!(request.worker_id, "agent-2");
        assert_eq!(request.tool_name, "Bash");
        assert_eq!(request.tool_use_id, "toolu_1");
        assert_eq!(pending_tool_name, "Bash");
        assert_eq!(pending_tool_use_id, "toolu_1");
        assert_eq!(pending_description, "Run command?");
        assert!(!mailbox_sent);
    }

    #[test]
    fn apply_pending_worker_request_writes_app_state() {
        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        let outcome = handle_swarm_worker_permission(params());
        apply_pending_worker_request_from_outcome(&store, &outcome);
        let pending = store.get().pending_worker_request.clone().unwrap();
        assert_eq!(pending.tool_name, "Bash");
        assert_eq!(pending.tool_use_id, "toolu_1");
        assert_eq!(pending.description, "Run command?");
        clear_pending_worker_request(&store);
        assert!(store.get().pending_worker_request.is_none());
    }

    #[test]
    fn begin_pending_sandbox_request_registers_callback_and_sets_app_state() {
        let _permission_lock = crate::utils::swarm::permission_sync::TEST_PERMISSION_SYNC_LOCK
            .lock()
            .unwrap();
        let _team_state_lock = crate::utils::swarm::team_helpers::TEST_TEAM_HELPERS_LOCK
            .lock()
            .unwrap();
        let _mailbox_lock = crate::utils::teammate_mailbox::TEST_TEAMMATE_MAILBOX_LOCK
            .lock()
            .unwrap();
        let _teammate_lock = crate::utils::teammate::TEST_TEAMMATE_CONTEXT_LOCK
            .lock()
            .unwrap();
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::swarm::permission_sync::clear_permission_sync_for_test();
        crate::utils::teammate_mailbox::clear_mailboxes_for_test();
        let _callback_lock = crate::hooks::use_swarm_permission_poller::TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::hooks::use_swarm_permission_poller::clear_sandbox_permission_callbacks_for_test();
        crate::utils::process_env::set("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
        crate::utils::teammate::set_dynamic_team_context(Some(
            crate::utils::teammate::DynamicTeamContext {
                agent_id: "worker-1".into(),
                agent_name: "worker".into(),
                team_name: "alpha".into(),
                color: None,
                plan_mode_required: false,
                parent_session_id: None,
            },
        ));
        // The sandbox request routes through `permission_sync::get_leader_name`,
        // which reads the team file from disk (CC `permissionSync.ts:657`
        // `readTeamFileAsync`). A memory-only seed would be a no-op fixture, so
        // pin a scratch config root and enable the team-file/mailbox disk paths.
        let scratch_root = std::env::temp_dir().join(format!(
            "cometix-swarm-worker-handler-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &scratch_root);
        let _io_guard = EnvVarGuard::set("COMETIX_TEST_TEAM_FILE_IO", "1");
        let _write_guard = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        crate::utils::swarm::team_helpers::clear_team_tool_state_for_test();
        let record = crate::utils::swarm::team_helpers::create_team_record(
            "alpha".into(),
            None,
            Some(crate::utils::swarm::constants::TEAM_LEAD_NAME.to_string()),
            None,
            "/tmp".into(),
        );
        crate::utils::swarm::team_helpers::write_team_record(record);

        let store = AppStore::new(crate::state::app_state_store::AppState::default(), None);
        let rx = begin_pending_sandbox_request(&store, "api.example.com").expect("worker path");
        let pending = store.get().pending_sandbox_request.clone().unwrap();
        assert_eq!(pending.host, "api.example.com");
        assert!(
            crate::hooks::use_swarm_permission_poller::has_sandbox_permission_callback(
                &pending.request_id
            )
        );
        assert!(
            crate::hooks::use_swarm_permission_poller::process_sandbox_permission_response(
                &pending.request_id,
                "api.example.com",
                true,
            )
        );
        assert!(rx.try_recv().ok() == Some(true));
        crate::hooks::use_inbox_poller::clear_pending_sandbox_request(&store);
        assert!(store.get().pending_sandbox_request.is_none());

        crate::utils::teammate::set_dynamic_team_context(None);
        crate::utils::process_env::remove("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS");
    }
}
