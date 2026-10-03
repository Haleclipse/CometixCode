//! Maps to: CC `hooks/toolPermission/PermissionContext.ts`.
//! Owns permission queue operations, resolve-once races, and the
//! `persistPermissions` state/persistence boundary used by interactive
//! decisions. The concrete queue remains in `REPL`, as React state does in CC.

use crate::types::permissions::{PermissionRequest, PermissionUpdate, ToolUseConfirm};
use std::sync::{Arc, Mutex};

/// Maps to: CC `PermissionContext.ts#PermissionApprovalSource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionApprovalSource {
    Hook { permanent: Option<bool> },
    User { permanent: bool },
    Classifier,
}

/// Maps to: CC `PermissionContext.ts#PermissionRejectionSource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionRejectionSource {
    Hook,
    UserAbort,
    UserReject { has_feedback: bool },
}

/// Maps to: CC `permissionLogging.ts#PermissionDecisionArgs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDecisionLogArgs {
    Accept(PermissionApprovalSource),
    AcceptConfig,
    Reject(PermissionRejectionSource),
    RejectConfig,
}

/// Patch payload for queue updates.
/// Maps to: CC `PermissionQueueOps.update(toolUseID, patch)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolUseConfirmPatch {
    pub request: Option<PermissionRequest>,
    pub permission_prompt_start_time_ms: Option<Option<i64>>,
    pub classifier_check_in_progress: Option<bool>,
}

pub fn push_to_queue(queue: &mut Vec<ToolUseConfirm>, item: ToolUseConfirm) {
    queue.push(item);
}

pub fn remove_from_queue(queue: &mut Vec<ToolUseConfirm>, tool_use_id: &str) {
    queue.retain(|item| item.tool_use_id() != tool_use_id);
}

pub fn update_queue_item(
    queue: &mut Vec<ToolUseConfirm>,
    tool_use_id: &str,
    patch: ToolUseConfirmPatch,
) {
    for item in queue
        .iter_mut()
        .filter(|item| item.tool_use_id() == tool_use_id)
    {
        if let Some(request) = &patch.request {
            item.request = request.clone();
        }
        if let Some(permission_prompt_start_time_ms) = patch.permission_prompt_start_time_ms {
            item.permission_prompt_start_time_ms = permission_prompt_start_time_ms;
        }
        if let Some(classifier_check_in_progress) = patch.classifier_check_in_progress {
            item.classifier_check_in_progress = classifier_check_in_progress;
        }
    }
}

/// Maps to CC `PermissionContext.ts#createPermissionContext(...).persistPermissions`.
/// Persist editable destinations before projecting every update into the live
/// permission context. CC ignores ordinary settings-writer failures, so those
/// still publish live updates. Only an actual propagated error (the explicit
/// Cometix no-write gate) leaves the context unchanged.
pub fn persist_permissions(
    app_store: &crate::state::store::AppStore,
    updates: &[PermissionUpdate],
) -> anyhow::Result<bool> {
    if updates.is_empty() {
        return Ok(false);
    }
    crate::utils::permissions::permission_update::persist_permission_updates(updates)?;
    let next = crate::utils::permissions::permission_update::apply_permission_updates(
        &app_store.tool_permission_context(),
        updates,
    );
    app_store.set_tool_permission_context(next);
    Ok(updates.iter().any(|update| {
        let destination = match update {
            PermissionUpdate::SetMode { destination, .. }
            | PermissionUpdate::AddRules { destination, .. }
            | PermissionUpdate::ReplaceRules { destination, .. }
            | PermissionUpdate::RemoveRules { destination, .. }
            | PermissionUpdate::AddDirectories { destination, .. }
            | PermissionUpdate::RemoveDirectories { destination, .. } => *destination,
        };
        crate::utils::permissions::permission_update::supports_persistence(destination)
    }))
}

/// Maps to: CC `PermissionContext.ts:154-173`
/// `createPermissionContext(...).cancelAndAbort(feedback, isAbort, contentBlocks)`
/// — the rejection decision every permission dialog resolves with.
///
/// ```text
/// const sub = !!toolUseContext.agentId
/// const baseMessage = feedback
///   ? `${sub ? SUBAGENT_REJECT_MESSAGE_WITH_REASON_PREFIX : REJECT_MESSAGE_WITH_REASON_PREFIX}${feedback}`
///   : sub ? SUBAGENT_REJECT_MESSAGE : REJECT_MESSAGE
/// const message = sub ? baseMessage : withMemoryCorrectionHint(baseMessage)
/// if (isAbort || (!feedback && !contentBlocks?.length && !sub)) { …; abort() }
/// return { behavior: 'ask', message, contentBlocks }
/// ```
///
/// Two behaviors ride on `sub` besides the copy, and both are ported here:
/// `withMemoryCorrectionHint` wraps only the main-loop branch, and the `!sub`
/// term in the abort condition means a SUBAGENT rejection never aborts the
/// controller — the subagent is expected to keep working around the denial.
///
/// `feedback` is JS-truthy-tested, so an empty string behaves like absence in
/// both the prefix choice and the abort condition.
///
/// Returns the decision's `message`. CC's `contentBlocks` travel separately in
/// this port (`PermissionPromptResponse::content_blocks`), and the
/// `behavior: 'ask'` envelope is what
/// `services/tools/tool_execution.rs#permission_terminal_result_for_decision`
/// turns into the model-facing `tool_result` (CC `toolExecution.ts:1023`).
pub fn cancel_and_abort(
    tool_name: &str,
    tool_use_context: &crate::tool::ToolUseContext,
    feedback: Option<&str>,
    is_abort: bool,
    content_blocks: &[crate::types::permissions::PermissionContentBlock],
) -> String {
    let sub = tool_use_context.agent_id.is_some();
    let feedback = feedback.filter(|feedback| !feedback.is_empty());
    let message = cancel_and_abort_message(sub, feedback);
    if is_abort || (feedback.is_none() && content_blocks.is_empty() && !sub) {
        crate::utils::debug::log_for_debugging(&format!(
            "Aborting: tool={tool_name} isAbort={is_abort} hasFeedback={} isSubagent={sub}",
            feedback.is_some()
        ));
        tool_use_context.abort_controller.abort();
    }
    message
}

/// The `baseMessage`/`message` half of [`cancel_and_abort`]
/// (CC `PermissionContext.ts:160-165`).
///
/// Split from the abort only because this port funnels three different CC
/// sites through one `tool_result` builder: the dialog rejection (CC's only
/// `cancelAndAbort` caller, which aborts), the permission SYSTEM's own deny
/// (`toolExecution.ts:1023`, which does not), and a Rust-side compat re-check.
/// The abort therefore stays with the dialog seam that owns it.
pub(crate) fn cancel_and_abort_message(sub: bool, feedback: Option<&str>) -> String {
    let base_message = match (feedback, sub) {
        (Some(feedback), true) => format!(
            "{}{feedback}",
            crate::utils::messages::SUBAGENT_REJECT_MESSAGE_WITH_REASON_PREFIX
        ),
        (Some(feedback), false) => format!(
            "{}{feedback}",
            crate::utils::messages::REJECT_MESSAGE_WITH_REASON_PREFIX
        ),
        (None, true) => crate::utils::messages::SUBAGENT_REJECT_MESSAGE.to_string(),
        (None, false) => crate::utils::messages::REJECT_MESSAGE.to_string(),
    };
    if sub {
        base_message
    } else {
        crate::utils::messages::with_memory_correction_hint(&base_message)
    }
}

/// Resolve-once guard for async permission races.
/// Maps to: CC `PermissionContext.ts:63-94` `createResolveOnce(...)`.
///
/// CC's factory takes the pending promise's `resolve` and hands the value
/// straight to it; this port has no promise to close over at construction, so
/// the value is parked here and the winning racer `take()`s it. The consumer is
/// `types/permissions.rs#PermissionPromptResponder`, which pairs one of these
/// with the transport the asking actor is parked on — CC's
/// `const { resolve: resolveOnce, isResolved, claim } = createResolveOnce(resolve)`
/// at `interactiveHandler.ts:70`, except that this port's racers reach the
/// guard through clones of the queue row instead of a shared closure.
#[derive(Debug)]
pub struct ResolveOnce<T> {
    claimed: std::sync::atomic::AtomicBool,
    delivered: std::sync::atomic::AtomicBool,
    value: Arc<Mutex<Option<T>>>,
}

impl<T> Default for ResolveOnce<T> {
    fn default() -> Self {
        Self {
            claimed: std::sync::atomic::AtomicBool::new(false),
            delivered: std::sync::atomic::AtomicBool::new(false),
            value: Arc::new(Mutex::new(None)),
        }
    }
}

impl<T> ResolveOnce<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_resolved(&self) -> bool {
        self.claimed.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn claim(&self) -> bool {
        self.claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    pub fn resolve(&self, value: T) -> bool {
        if self
            .delivered
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        self.claimed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *self.value.lock().unwrap() = Some(value);
        true
    }

    pub fn take(&self) -> Option<T> {
        self.value.lock().unwrap().take()
    }
}

pub fn create_resolve_once<T>() -> ResolveOnce<T> {
    ResolveOnce::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{
        PermissionBehavior, PermissionMode, PermissionRequest, PermissionRuleSource,
        PermissionRuleValue, PermissionUpdateDestination,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct TestEnvironmentGuard {
        previous_cwd: std::path::PathBuf,
        previous_original_cwd: std::path::PathBuf,
        previous_write: Option<EnvVarGuard>,
        temporary: std::path::PathBuf,
    }

    impl TestEnvironmentGuard {
        fn isolated_no_write() -> Self {
            let previous_cwd = std::env::current_dir().unwrap();
            let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
            let previous_write = Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0"));
            let temporary = std::env::temp_dir().join(format!(
                "cometix-permission-context-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&temporary).unwrap();
            // macOS exposes the temp root through `/var` -> `/private/var`;
            // use the real path so the production symlink-parent guard remains
            // exercised rather than weakened for this isolated test.
            let temporary = std::fs::canonicalize(temporary).unwrap();
            crate::bootstrap::state::set_original_cwd(&temporary);
            std::env::set_current_dir(&temporary).unwrap();
            Self {
                previous_cwd,
                previous_original_cwd,
                previous_write,
                temporary,
            }
        }
    }

    impl Drop for TestEnvironmentGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            drop(self.previous_write.take());
            let _ = std::fs::remove_dir_all(&self.temporary);
        }
    }

    fn request(id: &str) -> ToolUseConfirm {
        ToolUseConfirm::new(PermissionRequest {
            permission_result: None,
            id: format!("perm-{id}"),
            tool_use_id: id.to_string(),
            tool_name: "Bash".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: String::new(),
            message: String::new(),
            input_summary: "echo permission-gated".to_string(),
            input: serde_json::json!({ "command": "echo permission-gated" }),
            call_input: None,
            rule: PermissionRuleValue::new("Bash", Some("echo permission-gated".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        })
    }

    #[test]
    fn queue_ops_push_update_and_remove_by_tool_use_id() {
        let mut queue = Vec::new();
        push_to_queue(&mut queue, request("toolu_1"));
        push_to_queue(&mut queue, request("toolu_2"));

        update_queue_item(
            &mut queue,
            "toolu_2",
            ToolUseConfirmPatch {
                permission_prompt_start_time_ms: Some(Some(1234)),
                classifier_check_in_progress: Some(true),
                ..ToolUseConfirmPatch::default()
            },
        );

        assert_eq!(queue[1].permission_prompt_start_time_ms, Some(1234));
        assert!(queue[1].classifier_check_in_progress);

        remove_from_queue(&mut queue, "toolu_1");

        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].tool_use_id(), "toolu_2");
    }

    #[test]
    fn persist_permissions_matches_official_live_projection_after_caught_disk_failure() {
        let _environment = TEST_ENV_LOCK.lock().unwrap();
        let _guard = TestEnvironmentGuard::isolated_no_write();

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let session_rule = PermissionRuleValue::new("Bash", Some("git status".to_string()));
        let session = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::Session,
            behavior: PermissionBehavior::Allow,
            rules: vec![session_rule.clone()],
        };
        assert!(!persist_permissions(&store, &[session]).unwrap());
        assert!(
            store
                .tool_permission_context()
                .always_allow_rules
                .get(&PermissionRuleSource::Session)
                .is_some_and(|rules| rules.contains(&session_rule))
        );

        let local_rule = PermissionRuleValue::new("Bash", Some("cargo test".to_string()));
        let local = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![local_rule],
        };
        let error = persist_permissions(&store, &[local]).unwrap_err();
        assert_eq!(
            error.to_string(),
            crate::tools::shared::write_gate::PERMISSION_PERSISTENCE_DISABLED_ERROR
        );
        assert!(
            !store
                .tool_permission_context()
                .always_allow_rules
                .contains_key(&PermissionRuleSource::LocalSettings)
        );

        crate::utils::process_env::set("COMETIX_WRITE_ENABLED", "1");
        let persisted_rule = PermissionRuleValue::new("Bash", Some("cargo check".to_string()));
        let persisted = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![persisted_rule.clone()],
        };
        let settings_before_persist = store.get().settings.clone();
        assert!(persist_permissions(&store, &[persisted]).unwrap());
        assert!(
            store
                .tool_permission_context()
                .always_allow_rules
                .get(&PermissionRuleSource::LocalSettings)
                .is_some_and(|rules| rules.contains(&persisted_rule))
        );
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(".claude/settings.local.json").unwrap())
                .unwrap();
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!(["Bash(cargo check)"])
        );
        assert_eq!(
            store.get().settings,
            settings_before_persist,
            "persistPermissions updates the live permission context directly without reloading AppState.settings",
        );

        std::fs::remove_dir_all(".claude").unwrap();
        std::fs::create_dir_all(".claude/settings.local.json").unwrap();
        let failed_rule = PermissionRuleValue::new("Bash", Some("cargo clippy".to_string()));
        let disk_failure = PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![failed_rule.clone()],
        };
        // CC PermissionContext.ts:141-145 and PermissionUpdate.ts:235-242:
        // addPermissionRulesToSettings catches the directory/write failure and
        // returns false; its caller ignores that boolean before live projection.
        assert!(persist_permissions(&store, &[disk_failure]).unwrap());
        assert!(
            store
                .tool_permission_context()
                .always_allow_rules
                .get(&PermissionRuleSource::LocalSettings)
                .is_some_and(|rules| rules.contains(&failed_rule))
        );
        assert!(std::path::Path::new(".claude/settings.local.json").is_dir());
    }

    fn context_for(agent_id: Option<&str>) -> crate::tool::ToolUseContext {
        crate::tool::ToolUseContext {
            agent_id: agent_id.map(str::to_string),
            ..crate::tool::ToolUseContext::default()
        }
    }

    /// Maps to: CC `hooks/toolPermission/PermissionContext.ts:159-165`.
    ///
    /// ```ts
    /// const sub = !!toolUseContext.agentId
    /// const baseMessage = feedback
    ///   ? `${sub ? SUBAGENT_REJECT_MESSAGE_WITH_REASON_PREFIX : REJECT_MESSAGE_WITH_REASON_PREFIX}${feedback}`
    ///   : sub ? SUBAGENT_REJECT_MESSAGE : REJECT_MESSAGE
    /// const message = sub ? baseMessage : withMemoryCorrectionHint(baseMessage)
    /// ```
    ///
    /// The four constants are `utils/messages.ts:212-219`. A branch that always
    /// picks one side fails on the pair below.
    #[test]
    fn cancel_and_abort_matches_official_subagent_and_main_loop_reject_copy() {
        let main = context_for(None);
        let sub = context_for(Some("agent-1"));

        // `sub ? SUBAGENT_REJECT_MESSAGE : REJECT_MESSAGE` (`:162-164`).
        assert_eq!(
            cancel_and_abort(BASH, &main, None, false, &[]),
            crate::utils::messages::with_memory_correction_hint(
                crate::utils::messages::REJECT_MESSAGE
            )
        );
        assert_eq!(
            cancel_and_abort(BASH, &sub, None, false, &[]),
            crate::utils::messages::SUBAGENT_REJECT_MESSAGE
        );
        assert_ne!(
            cancel_and_abort(BASH, &main, None, false, &[]),
            cancel_and_abort(BASH, &sub, None, false, &[])
        );

        // The `_WITH_REASON_PREFIX` pair (`:161`).
        assert_eq!(
            cancel_and_abort(BASH, &main, Some("use ls"), false, &[]),
            crate::utils::messages::with_memory_correction_hint(&format!(
                "{}use ls",
                crate::utils::messages::REJECT_MESSAGE_WITH_REASON_PREFIX
            ))
        );
        assert_eq!(
            cancel_and_abort(BASH, &sub, Some("use ls"), false, &[]),
            format!(
                "{}use ls",
                crate::utils::messages::SUBAGENT_REJECT_MESSAGE_WITH_REASON_PREFIX
            )
        );

        // `feedback ? …` is JS truthiness, so `''` takes the no-feedback branch.
        assert_eq!(
            cancel_and_abort(BASH, &sub, Some(""), false, &[]),
            crate::utils::messages::SUBAGENT_REJECT_MESSAGE
        );

        // `withMemoryCorrectionHint` wraps ONLY the non-subagent branch
        // (`:165`). The switch is off in this build (CC's own GrowthBook
        // fallback, `utils/messages.ts:188`), so the identity is asserted
        // through the function rather than by re-spelling the hint.
        assert_eq!(
            cancel_and_abort_message(true, None),
            crate::utils::messages::SUBAGENT_REJECT_MESSAGE
        );
    }

    /// Maps to: CC `PermissionContext.ts:166-171`
    /// `if (isAbort || (!feedback && !contentBlocks?.length && !sub)) { … abort() }`.
    ///
    /// The `!sub` term is behavior, not copy: a subagent rejection must leave
    /// the controller running so the subagent can work around the denial.
    #[test]
    fn cancel_and_abort_matches_official_abort_condition() {
        // Main loop, no feedback, no content blocks -> abort.
        let main = context_for(None);
        cancel_and_abort(BASH, &main, None, false, &[]);
        assert!(main.abort_controller.is_aborted());

        // Same rejection from a SUBAGENT -> `!sub` is false, no abort.
        let sub = context_for(Some("agent-1"));
        cancel_and_abort(BASH, &sub, None, false, &[]);
        assert!(!sub.abort_controller.is_aborted());

        // Feedback present -> the user is steering, not interrupting.
        let with_feedback = context_for(None);
        cancel_and_abort(BASH, &with_feedback, Some("use ls"), false, &[]);
        assert!(!with_feedback.abort_controller.is_aborted());

        // Content blocks present (a pasted image) -> same.
        let with_blocks = context_for(None);
        cancel_and_abort(
            BASH,
            &with_blocks,
            None,
            false,
            &[
                crate::types::permissions::PermissionContentBlock::image_base64(
                    "image/png",
                    "AAAA",
                ),
            ],
        );
        assert!(!with_blocks.abort_controller.is_aborted());

        // `isAbort` short-circuits every other term, including `!sub`
        // (`interactiveHandler.ts:152` `ctx.cancelAndAbort(undefined, true)`).
        let aborted_sub = context_for(Some("agent-1"));
        cancel_and_abort(BASH, &aborted_sub, Some("use ls"), true, &[]);
        assert!(aborted_sub.abort_controller.is_aborted());
    }

    const BASH: &str = "Bash";

    #[test]
    fn resolve_once_matches_official_claim_and_single_delivery_semantics() {
        let resolve_once = create_resolve_once::<&'static str>();
        assert!(!resolve_once.is_resolved());
        assert!(resolve_once.claim());
        assert!(resolve_once.is_resolved());
        assert!(!resolve_once.claim());

        let resolve_once = create_resolve_once::<&'static str>();
        assert!(resolve_once.resolve("first"));
        assert!(!resolve_once.resolve("second"));
        assert_eq!(resolve_once.take(), Some("first"));
    }
}
