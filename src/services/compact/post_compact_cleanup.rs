//! Post-compaction cache and tracking cleanup.
//!
//! Maps to: CC `services/compact/postCompactCleanup.ts:1-75`.
//!
//! This owner is shared by manual compact, auto-compact, teammate compact, and
//! `/clear`. Main-thread-only caches are protected from subagent compaction in
//! the same way as CC's `querySource.startsWith('repl_main_thread')` guard.

use crate::constants::query_source::QuerySource;

/// Maps to CC `postCompactCleanup.ts:37-39` main-thread source predicate.
pub fn is_main_thread_compact(query_source: Option<&QuerySource>) -> bool {
    query_source.is_none_or(|source| {
        source.as_api_source().starts_with("repl_main_thread") || matches!(source, QuerySource::Sdk)
    })
}

/// Maps to: CC `services/compact/postCompactCleanup.ts:31-75`
/// `runPostCompactCleanup(querySource?)`.
pub fn run_post_compact_cleanup(query_source: Option<&QuerySource>) {
    let is_main_thread = is_main_thread_compact(query_source);

    crate::services::compact::micro_compact::reset_microcompact_state();

    if is_main_thread {
        if crate::services::context_collapse::is_context_collapse_enabled() {
            crate::services::context_collapse::reset_context_collapse();
        }
        // Clear both the memoized outer user-context layer and the underlying
        // memory-file discovery cache. Clearing only one leaves stale AGENTS.md
        // bytes reachable on the next turn.
        crate::context::clear_user_context_cache();
        crate::utils::agents_md::reset_get_memory_files_cache("compact");
    }

    crate::constants::system_prompt_sections::clear_system_prompt_sections();
    crate::utils::classifier_approvals::clear_classifier_approvals();
    crate::tools::bash_tool::bash_permissions::clear_speculative_checks();

    // Deliberate source-backed omissions:
    // - beta tracing / telemetry is intentionally omitted project-wide;
    // - attributionHooks.sweepFileContentCache has no live attribution cache;
    crate::utils::session_storage::clear_session_messages_cache();
    // As in CC, invoked skill content is intentionally not cleared.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_shared_cleanup_state() {
        crate::bootstrap::state::set_system_prompt_section_cache_entry(
            "test-section",
            Some("cached".to_string()),
        );
        crate::context::seed_context_caches_for_test();
        crate::utils::agents_md::seed_memory_files_cache_for_test();
        crate::utils::classifier_approvals::set_classifier_approval(
            "toolu-cleanup",
            "Bash(echo *)",
        );
        crate::services::compact::micro_compact::pin_cache_edits(
            0,
            crate::services::compact::micro_compact::CacheEditsBlock::delete_refs(vec![
                "toolu-old".to_string(),
            ]),
        );
    }

    #[test]
    fn main_thread_cleanup_matches_official_cache_ownership() {
        let _micro_lock = crate::services::compact::micro_compact::TEST_CACHED_MC_LOCK
            .lock()
            .unwrap();
        seed_shared_cleanup_state();

        run_post_compact_cleanup(Some(&QuerySource::Prompt));

        assert_eq!(
            crate::bootstrap::state::system_prompt_section_cache_len_for_test(),
            0
        );
        assert_eq!(
            crate::context::context_cache_presence_for_test(),
            (false, true)
        );
        assert!(!crate::utils::agents_md::memory_files_cache_is_populated_for_test());
        assert!(
            crate::utils::classifier_approvals::get_classifier_approval("toolu-cleanup").is_none()
        );
        assert!(
            crate::services::compact::micro_compact::cached_microcompact_state_for_test()
                .pinned_edits
                .is_empty()
        );
        crate::context::clear_system_context_cache();
    }

    #[test]
    fn subagent_cleanup_preserves_main_user_and_memory_caches_only() {
        let _micro_lock = crate::services::compact::micro_compact::TEST_CACHED_MC_LOCK
            .lock()
            .unwrap();
        seed_shared_cleanup_state();

        run_post_compact_cleanup(Some(&QuerySource::Agent));

        // These three stores are process-global but belong to the main thread.
        assert_eq!(
            crate::context::context_cache_presence_for_test(),
            (true, true)
        );
        assert!(crate::utils::agents_md::memory_files_cache_is_populated_for_test());
        // System-prompt, classifier, speculative, and microcompact state are
        // cleared for every compact in the source.
        assert_eq!(
            crate::bootstrap::state::system_prompt_section_cache_len_for_test(),
            0
        );
        assert!(
            crate::utils::classifier_approvals::get_classifier_approval("toolu-cleanup").is_none()
        );
        assert!(
            crate::services::compact::micro_compact::cached_microcompact_state_for_test()
                .pinned_edits
                .is_empty()
        );

        crate::context::clear_user_context_cache();
        crate::context::clear_system_context_cache();
        crate::utils::agents_md::reset_get_memory_files_cache("test_cleanup");
    }

    #[test]
    fn cleanup_intentionally_preserves_invoked_skill_content() {
        let _micro_lock = crate::services::compact::micro_compact::TEST_CACHED_MC_LOCK
            .lock()
            .unwrap();
        let _skills_lock = crate::bootstrap::state::TEST_INVOKED_SKILLS_LOCK
            .lock()
            .unwrap();
        crate::bootstrap::state::clear_invoked_skills(None);
        crate::bootstrap::state::add_invoked_skill(
            "review",
            "/skills/review/SKILL.md",
            "Review carefully",
            None,
        );

        run_post_compact_cleanup(Some(&QuerySource::Prompt));

        assert_eq!(
            crate::bootstrap::state::get_invoked_skills_for_agent(None).len(),
            1
        );
        crate::bootstrap::state::clear_invoked_skills(None);
    }

    #[test]
    fn sdk_is_the_only_non_repl_source_treated_as_main_thread() {
        assert!(is_main_thread_compact(None));
        assert!(is_main_thread_compact(Some(&QuerySource::Prompt)));
        assert!(is_main_thread_compact(Some(&QuerySource::Sdk)));
        assert!(!is_main_thread_compact(Some(&QuerySource::Agent)));
        assert!(!is_main_thread_compact(Some(&QuerySource::Compact)));
        assert!(!is_main_thread_compact(Some(&QuerySource::SessionMemory)));
    }
}
