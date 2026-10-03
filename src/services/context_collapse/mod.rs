//! Context-collapse service seam.
//! Maps to CC `services/contextCollapse/index.ts`.
//!
//! The checked-in upstream `services/contextCollapse/index.ts` is currently a
//! generated empty stub, while `query.ts` still contains the optional
//! `contextCollapse.applyCollapsesIfNeeded(...)` control-flow slot before
//! autocompact. Cometix keeps this as a safe no-op seam at that official query
//! position until a non-stub upstream implementation exists.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCollapseResult {
    /// Maps to official `applyCollapsesIfNeeded(...).messages`.
    pub messages: Vec<crate::types::message::Message>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCollapseOverflowRecovery {
    /// Maps to official `recoverFromOverflow(...).messages`.
    pub messages: Vec<crate::types::message::Message>,
    /// Maps to official `recoverFromOverflow(...).committed`.
    pub committed: usize,
}

/// Maps to: CC `services/contextCollapse/index.ts` `isContextCollapseEnabled()`.
pub fn is_context_collapse_enabled() -> bool {
    crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("COMETIX_CONTEXT_COLLAPSE").as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_CONTEXT_COLLAPSE")
            .as_deref(),
    ) || crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CONTEXT_COLLAPSE").as_deref(),
    )
}

/// Maps to the `resetContextCollapse()` call made by CC
/// `services/compact/postCompactCleanup.ts:42-48`.
///
/// The checked-in `services/contextCollapse/index.ts` is a generated empty
/// stub and Cometix therefore has no process-wide collapse store to mutate.
/// Keep the reset boundary explicit so the real store has one owner when a
/// source-backed implementation becomes available.
pub fn reset_context_collapse() {}

/// Maps to: CC `services/contextCollapse/index.ts` `applyCollapsesIfNeeded(...)`.
pub fn apply_collapses_if_needed(
    messages_for_query: Vec<crate::types::message::Message>,
    _tool_use_context: &crate::tool::ToolUseContext,
    _query_source: &crate::constants::query_source::QuerySource,
) -> ContextCollapseResult {
    // Store-backed context collapse is not ported yet. Keep this as an
    // explicit no-op at the official control-flow point so autocompact receives
    // the projected view once the service is implemented.
    ContextCollapseResult {
        messages: messages_for_query,
    }
}

/// Maps to: CC `query.ts` optional
/// `contextCollapse.isWithheldPromptTooLong(...)` check.
pub fn is_withheld_prompt_too_long(
    error: &crate::types::message::SystemApiErrorMessage,
    _query_source: &crate::constants::query_source::QuerySource,
) -> bool {
    is_context_collapse_enabled()
        && (error
            .content
            .starts_with(crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE)
            || error
                .error_details
                .as_deref()
                .is_some_and(|details| details.to_ascii_lowercase().contains("prompt is too long")))
}

/// Maps to: CC `query.ts` optional `contextCollapse.recoverFromOverflow(...)`.
pub fn recover_from_overflow(
    messages_for_query: Vec<crate::types::message::Message>,
    _query_source: &crate::constants::query_source::QuerySource,
) -> ContextCollapseOverflowRecovery {
    // Store-backed staged collapse draining is not ported yet. Returning
    // `committed = 0` mirrors the official no-recovery path while preserving
    // the query-loop branch that must run before reactive compact/stop hooks.
    ContextCollapseOverflowRecovery {
        messages: messages_for_query,
        committed: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolPermissionContext;
    use crate::tool::ToolUseContext;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn context_collapse_enabled_honors_official_env_alias() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("COMETIX_CONTEXT_COLLAPSE");
        crate::utils::process_env::remove("CLAUDE_CODE_CONTEXT_COLLAPSE");
        crate::utils::process_env::remove("CLAUDE_CONTEXT_COLLAPSE");
        assert!(!is_context_collapse_enabled());

        crate::utils::process_env::set("CLAUDE_CONTEXT_COLLAPSE", "1");
        assert!(is_context_collapse_enabled());
        crate::utils::process_env::remove("CLAUDE_CONTEXT_COLLAPSE");
    }

    #[test]
    fn withheld_prompt_too_long_and_recover_overflow_are_safe_noops_until_store_is_ported() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CONTEXT_COLLAPSE", "1");
        let error = crate::types::message::SystemApiErrorMessage {
            content: crate::services::api::errors::PROMPT_TOO_LONG_ERROR_MESSAGE.to_string(),
            api_error: "invalid_request".to_string(),
            error: "invalid_request".to_string(),
            error_details: Some("prompt is too long: over limit".to_string()),
        };
        assert!(is_withheld_prompt_too_long(
            &error,
            &crate::constants::query_source::QuerySource::Prompt,
        ));

        let messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "before overflow".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )];
        let recovered = recover_from_overflow(
            messages.clone(),
            &crate::constants::query_source::QuerySource::Prompt,
        );
        assert_eq!(recovered.messages, messages);
        assert_eq!(recovered.committed, 0);
        crate::utils::process_env::remove("CLAUDE_CONTEXT_COLLAPSE");
    }

    #[test]
    fn apply_collapses_if_needed_is_safe_noop_until_store_is_ported() {
        let messages = vec![crate::types::message::Message::User(
            crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "before collapse".to_string(),
                )],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            },
        )];
        let context = ToolUseContext::with_permission_context(ToolPermissionContext::default())
            .with_messages(messages.clone());

        let result = apply_collapses_if_needed(
            messages.clone(),
            &context,
            &crate::constants::query_source::QuerySource::Prompt,
        );

        assert_eq!(result.messages, messages);
    }
}
