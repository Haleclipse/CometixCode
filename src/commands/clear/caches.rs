//! Session cache clearing utilities.
//!
//! Maps to: CC `commands/clear/caches.ts`.
//!
//! Keep this module light: it is imported from the `/clear` path (and later
//! `--resume` / `--continue`). Call only helpers that already exist; missing
//! CC clears are documented inline and landed with their owning modules.

use std::collections::HashSet;

/// Maps to: CC `clearSessionCaches(preservedAgentIds)`.
///
/// Clears session-scoped caches without touching messages/session ID/hooks.
/// When `preserved_agent_ids` is non-empty, requestId-keyed state (pending
/// permission callbacks, dump/cache-break tracking) is left intact because it
/// cannot be safely scoped to the main session (CC comment).
pub fn clear_session_caches(preserved_agent_ids: &HashSet<String>) {
    let has_preserved = !preserved_agent_ids.is_empty();

    // CC: getUserContext / getSystemContext memo caches.
    crate::context::clear_user_context_cache();
    crate::context::clear_system_context_cache();
    // getGitStatus / getSessionStartDate memo caches are not yet represented.

    // Maps to: CC `clearFileSuggestionCaches`.
    crate::hooks::file_suggestions::clear_file_suggestion_caches();
    crate::utils::suggestions::directory_completion::clear_path_cache();

    // CC: clearCommandsCache then clearDynamicSkills. A new conversation must
    // rediscover nested Read-triggered skills instead of inheriting them.
    crate::commands::clear_commands_cache();
    crate::skills::load_skills_dir::clear_dynamic_skills();

    // CC: resetPromptCacheBreakDetection — not ported.

    // Clear system prompt injection (cache breaker).
    crate::context::set_system_prompt_injection(None);

    // CC: setLastEmittedDate(null) — bootstrap date emit tracking not ported.

    crate::services::compact::post_compact_cleanup::run_post_compact_cleanup(None);

    // `/clear` uses the stronger session-start reason after compact cleanup;
    // the current cache owner treats both reasons identically while the future
    // InstructionsLoaded hook owner may distinguish them.
    crate::utils::claudemd::reset_get_memory_files_cache("session_start");

    crate::utils::attachments::reset_sent_skill_names();
    crate::bootstrap::state::clear_invoked_skills(Some(preserved_agent_ids));

    crate::utils::image_store::clear_stored_image_paths();

    // CC: clearAllSessions (sessionIngress) — not ported.

    // Clear swarm permission pending callbacks (Batch 5d residual).
    if !has_preserved {
        crate::hooks::use_swarm_permission_poller::clear_all_pending_callbacks();
        // Maps to CC `clearAllDumpState()` from dumpPrompts.ts.
        crate::services::api::dump_prompts::clear_all_dump_state();
        crate::services::api::dump_prompts::clear_api_request_cache();
    }

    // CC: Tungsten / COMMIT_ATTRIBUTION / repository / bash prefix / dump /
    // invoked skills / git dir / magic docs /
    // ToolSearch / agent definitions / SkillTool prompt — land with owners.

    crate::services::lsp::diagnostic_registry::reset_all_lsp_diagnostic_state();
    // CC `caches.ts:127` `clearSessionEnvVars()`: `/env` values end with the
    // conversation.
    crate::utils::session_env_vars::clear_session_env_vars();
    crate::tools::web_fetch_tool::utils::clear_web_fetch_cache();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::use_swarm_permission_poller::{
        PermissionResponseCallback, SandboxPermissionResponseCallback,
        clear_pending_callbacks_for_test, has_permission_callback, has_sandbox_permission_callback,
        register_permission_callback, register_sandbox_permission_callback,
    };
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::sync::Arc;

    #[test]
    fn clear_session_caches_clears_pending_callbacks_when_no_preserved() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _lock = crate::hooks::use_swarm_permission_poller::TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        clear_pending_callbacks_for_test();
        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-clear".into(),
            tool_use_id: "toolu".into(),
            on_allow: Arc::new(|_, _, _| {}),
            on_reject: Arc::new(|_| {}),
        });
        register_sandbox_permission_callback(SandboxPermissionResponseCallback {
            request_id: "sandbox-clear".into(),
            host: "h".into(),
            resolve: Arc::new(|_| {}),
        });
        assert!(has_permission_callback("perm-clear"));
        assert!(has_sandbox_permission_callback("sandbox-clear"));

        clear_session_caches(&HashSet::new());

        assert!(!has_permission_callback("perm-clear"));
        assert!(!has_sandbox_permission_callback("sandbox-clear"));
    }

    #[test]
    fn clear_session_caches_preserves_callbacks_when_agents_preserved() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _lock = crate::hooks::use_swarm_permission_poller::TEST_PENDING_CALLBACKS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        clear_pending_callbacks_for_test();
        register_permission_callback(PermissionResponseCallback {
            request_id: "perm-keep".into(),
            tool_use_id: "toolu".into(),
            on_allow: Arc::new(|_, _, _| {}),
            on_reject: Arc::new(|_| {}),
        });
        let mut preserved = HashSet::new();
        preserved.insert("agent-bg".into());
        clear_session_caches(&preserved);
        assert!(has_permission_callback("perm-keep"));
        clear_pending_callbacks_for_test();
    }

    #[test]
    fn clear_session_caches_resets_dynamic_and_conditional_skills() {
        let _env_lock = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _skills = crate::skills::load_skills_dir::DynamicSkillsTestSnapshot::capture();
        crate::skills::load_skills_dir::clear_dynamic_skills();
        let root = std::env::temp_dir().join(format!(
            "cometix-clear-dynamic-skills-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let skill = root.join("discovered");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: discovered\ndescription: Discovered\n---\nBody",
        )
        .unwrap();
        crate::skills::load_skills_dir::add_skill_directories(std::slice::from_ref(&root));
        assert!(!crate::skills::load_skills_dir::get_dynamic_skills().is_empty());

        clear_session_caches(&HashSet::new());

        assert!(crate::skills::load_skills_dir::get_dynamic_skills().is_empty());
        assert_eq!(
            crate::skills::load_skills_dir::get_conditional_skill_count(),
            0
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
