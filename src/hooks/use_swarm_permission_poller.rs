//! Maps to: CC `hooks/useSwarmPermissionPoller.ts`.
//!
//! Module-level registries for swarm-worker permission + sandbox callbacks.
//! Inbox mailbox responses invoke [`process_mailbox_permission_response`] /
//! [`process_sandbox_permission_response`]. File-poll responses use
//! [`process_permission_response`] (CC private `processResponse`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::types::permissions::PermissionUpdate;
use crate::utils::swarm::permission_sync::PermissionResponse;
#[cfg(test)]
use crate::utils::test_env::TestStateLock;

/// Maps to: CC `PermissionResponseCallback`.
pub struct PermissionResponseCallback {
    pub request_id: String,
    pub tool_use_id: String,
    pub on_allow:
        Arc<dyn Fn(Option<serde_json::Value>, Vec<PermissionUpdate>, Option<String>) + Send + Sync>,
    pub on_reject: Arc<dyn Fn(Option<String>) + Send + Sync>,
}

/// Maps to: CC `SandboxPermissionResponseCallback`.
pub struct SandboxPermissionResponseCallback {
    pub request_id: String,
    pub host: String,
    pub resolve: Arc<dyn Fn(bool) + Send + Sync>,
}

static PENDING_CALLBACKS: OnceLock<Mutex<HashMap<String, PermissionResponseCallback>>> =
    OnceLock::new();
static PENDING_SANDBOX_CALLBACKS: OnceLock<
    Mutex<HashMap<String, SandboxPermissionResponseCallback>>,
> = OnceLock::new();

fn pending_callbacks() -> &'static Mutex<HashMap<String, PermissionResponseCallback>> {
    PENDING_CALLBACKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_sandbox_callbacks() -> &'static Mutex<HashMap<String, SandboxPermissionResponseCallback>>
{
    PENDING_SANDBOX_CALLBACKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Maps to: CC `parsePermissionUpdates` — drop malformed entries.
fn parse_permission_updates(raw: Option<&serde_json::Value>) -> Vec<PermissionUpdate> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    crate::utils::permissions::permission_update_schema::permission_updates_from_official_json(raw)
}

fn parse_permission_updates_from_vec(raw: Option<&[serde_json::Value]>) -> Vec<PermissionUpdate> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    crate::utils::permissions::permission_update_schema::permission_updates_from_official_json(
        &serde_json::Value::Array(raw.to_vec()),
    )
}

/// Maps to: CC `registerPermissionCallback`.
pub fn register_permission_callback(callback: PermissionResponseCallback) {
    pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(callback.request_id.clone(), callback);
}

/// Maps to: CC `unregisterPermissionCallback`.
pub fn unregister_permission_callback(request_id: &str) {
    pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(request_id);
}

/// Maps to: CC `hasPermissionCallback`.
pub fn has_permission_callback(request_id: &str) -> bool {
    pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(request_id)
}

/// Maps to: CC `clearAllPendingCallbacks`.
pub fn clear_all_pending_callbacks() {
    pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    pending_sandbox_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Maps to: CC `processMailboxPermissionResponse`.
pub fn process_mailbox_permission_response(
    request_id: &str,
    decision: &str,
    feedback: Option<&str>,
    updated_input: Option<serde_json::Value>,
    permission_updates: Option<&serde_json::Value>,
) -> bool {
    let callback = pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(request_id);
    let Some(callback) = callback else {
        return false;
    };
    if decision == "approved" {
        let updates = parse_permission_updates(permission_updates);
        // CC processMailboxPermissionResponse does not forward feedback on allow.
        let _ = feedback;
        (callback.on_allow)(updated_input, updates, None);
    } else {
        (callback.on_reject)(feedback.map(str::to_string));
    }
    true
}

/// Maps to: CC private `processResponse` (file-poll path).
pub fn process_permission_response(response: &PermissionResponse) -> bool {
    let callback = pending_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&response.request_id);
    let Some(callback) = callback else {
        return false;
    };
    if response.decision == "approved" {
        let updates = parse_permission_updates_from_vec(response.permission_updates.as_deref());
        // CC processResponse does not forward feedback on allow.
        (callback.on_allow)(response.updated_input.clone(), updates, None);
    } else {
        (callback.on_reject)(response.feedback.clone());
    }
    true
}

/// Maps to: CC `registerSandboxPermissionCallback`.
pub fn register_sandbox_permission_callback(callback: SandboxPermissionResponseCallback) {
    pending_sandbox_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(callback.request_id.clone(), callback);
}

/// Maps to: CC `hasSandboxPermissionCallback`.
pub fn has_sandbox_permission_callback(request_id: &str) -> bool {
    pending_sandbox_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(request_id)
}

/// Maps to: CC `processSandboxPermissionResponse`.
pub fn process_sandbox_permission_response(request_id: &str, _host: &str, allow: bool) -> bool {
    let callback = pending_sandbox_callbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(request_id);
    let Some(callback) = callback else {
        return false;
    };
    (callback.resolve)(allow);
    true
}

/// Test helper: drop all registered callbacks (permission + sandbox).
#[cfg(test)]
pub fn clear_pending_callbacks_for_test() {
    clear_all_pending_callbacks();
}

/// Serializes tests that touch the module-level callback registries.
#[cfg(test)]
pub static TEST_PENDING_CALLBACKS_LOCK: TestStateLock = TestStateLock::new();

/// Test helper retained for existing callers.
#[cfg(test)]
pub fn clear_sandbox_permission_callbacks_for_test() {
    clear_all_pending_callbacks();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// CC useSwarmPermissionPoller.ts:35-57 validates a whole update before
    /// invoking either mailbox or disk-poll callbacks, preserving valid siblings.
    #[test]
    fn swarm_callbacks_match_official_whole_update_filtering() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all_pending_callbacks();
        let raw = serde_json::json!([
            {"type":"addRules","destination":"session","behavior":"allow",
                "rules":[{"toolName":"Bash","ruleContent":42}]},
            {"type":"addRules","destination":"session","behavior":"allow",
                "rules":[{"toolName":"Read"},{"toolName":"Bash","ruleContent":null}]},
            {"type":"addDirectories","destination":"session","directories":["/repo",42]},
            {"type":"addRules","destination":"session","behavior":"allow",
                "rules":[{"toolName":"Read","ruleContent":"src/**"}]}
        ]);
        for mailbox in [true, false] {
            let id = if mailbox {
                "schema-mailbox"
            } else {
                "schema-file"
            };
            let called = Arc::new(AtomicBool::new(false));
            let called_in_callback = called.clone();
            register_permission_callback(PermissionResponseCallback {
                request_id: id.into(),
                tool_use_id: "tool-schema".into(),
                on_allow: Arc::new(move |input, updates, feedback| {
                    assert!(!has_permission_callback(id), "removed before callback");
                    assert_eq!(input, Some(serde_json::json!({"file_path":"src/lib.rs"})));
                    assert!(feedback.is_none());
                    assert_eq!(crate::utils::permissions::permission_update_schema::permission_updates_to_official_json(&updates),
                        vec![serde_json::json!({"type":"addRules","destination":"session","behavior":"allow",
                            "rules":[{"toolName":"Read","ruleContent":"src/**"}]})]);
                    called_in_callback.store(true, Ordering::SeqCst);
                }),
                on_reject: Arc::new(|_| panic!("approved response still invokes allow")),
            });
            let input = Some(serde_json::json!({"file_path":"src/lib.rs"}));
            if mailbox {
                assert!(process_mailbox_permission_response(
                    id,
                    "approved",
                    Some("unused"),
                    input,
                    Some(&raw)
                ));
            } else {
                assert!(process_permission_response(&PermissionResponse {
                    request_id: id.into(),
                    decision: "approved".into(),
                    timestamp: "t".into(),
                    feedback: Some("unused".into()),
                    updated_input: input,
                    permission_updates: Some(raw.as_array().unwrap().clone()),
                }));
            }
            assert!(called.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn permission_callback_registry_registers_processes_and_clears() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_pending_callbacks_for_test();
        let allowed = Arc::new(AtomicBool::new(false));
        let allowed_flag = Arc::clone(&allowed);
        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-1".into(),
            tool_use_id: "toolu_1".into(),
            on_allow: Arc::new(move |input, updates, feedback| {
                assert_eq!(input, Some(serde_json::json!({"ok": true})));
                assert!(updates.is_empty());
                assert!(feedback.is_none());
                allowed_flag.store(true, Ordering::SeqCst);
            }),
            on_reject: Arc::new(|_| panic!("should not reject")),
        });
        assert!(has_permission_callback("perm-1"));
        assert!(process_mailbox_permission_response(
            "perm-1",
            "approved",
            Some("looks good"),
            Some(serde_json::json!({"ok": true})),
            None,
        ));
        assert!(allowed.load(Ordering::SeqCst));
        assert!(!has_permission_callback("perm-1"));
        assert!(!process_mailbox_permission_response(
            "perm-1", "approved", None, None, None,
        ));
    }

    #[test]
    fn permission_callback_reject_and_unregister() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_pending_callbacks_for_test();
        let rejected = Arc::new(AtomicBool::new(false));
        let rejected_flag = Arc::clone(&rejected);
        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-2".into(),
            tool_use_id: "toolu_2".into(),
            on_allow: Arc::new(|_, _, _| panic!("should not allow")),
            on_reject: Arc::new(move |feedback| {
                assert_eq!(feedback.as_deref(), Some("nope"));
                rejected_flag.store(true, Ordering::SeqCst);
            }),
        });
        assert!(process_mailbox_permission_response(
            "perm-2",
            "rejected",
            Some("nope"),
            None,
            None,
        ));
        assert!(rejected.load(Ordering::SeqCst));

        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-3".into(),
            tool_use_id: "toolu_3".into(),
            on_allow: Arc::new(|_, _, _| {}),
            on_reject: Arc::new(|_| {}),
        });
        unregister_permission_callback("perm-3");
        assert!(!has_permission_callback("perm-3"));
    }

    #[test]
    fn process_permission_response_from_file_poll() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_pending_callbacks_for_test();
        let seen = Arc::new(AtomicBool::new(false));
        let seen_flag = Arc::clone(&seen);
        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-file".into(),
            tool_use_id: "toolu_f".into(),
            on_allow: Arc::new(move |_, _, _| {
                seen_flag.store(true, Ordering::SeqCst);
            }),
            on_reject: Arc::new(|_| panic!("should not reject")),
        });
        assert!(process_permission_response(&PermissionResponse {
            request_id: "perm-file".into(),
            decision: "approved".into(),
            timestamp: "t".into(),
            feedback: None,
            updated_input: None,
            permission_updates: None,
        }));
        assert!(seen.load(Ordering::SeqCst));
    }

    #[test]
    fn sandbox_callback_registry_registers_processes_and_clears() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_sandbox_permission_callbacks_for_test();
        let seen = Arc::new(AtomicBool::new(false));
        let seen_flag = Arc::clone(&seen);
        register_sandbox_permission_callback(SandboxPermissionResponseCallback {
            request_id: "sandbox-1".into(),
            host: "api.example.com".into(),
            resolve: Arc::new(move |allow| {
                assert!(allow);
                seen_flag.store(true, Ordering::SeqCst);
            }),
        });
        assert!(has_sandbox_permission_callback("sandbox-1"));
        assert!(process_sandbox_permission_response(
            "sandbox-1",
            "api.example.com",
            true
        ));
        assert!(seen.load(Ordering::SeqCst));
        assert!(!has_sandbox_permission_callback("sandbox-1"));
        assert!(!process_sandbox_permission_response(
            "sandbox-1",
            "api.example.com",
            false
        ));
    }

    #[test]
    fn clear_all_pending_callbacks_clears_both_registries() {
        let _lock = TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_pending_callbacks_for_test();
        register_permission_callback(PermissionResponseCallback {
            request_id: "p".into(),
            tool_use_id: "t".into(),
            on_allow: Arc::new(|_, _, _| {}),
            on_reject: Arc::new(|_| {}),
        });
        register_sandbox_permission_callback(SandboxPermissionResponseCallback {
            request_id: "s".into(),
            host: "h".into(),
            resolve: Arc::new(|_| {}),
        });
        clear_all_pending_callbacks();
        assert!(!has_permission_callback("p"));
        assert!(!has_sandbox_permission_callback("s"));
    }
}
