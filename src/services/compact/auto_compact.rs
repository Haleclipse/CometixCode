//! Auto-compact service seam.
//! Maps to CC `services/compact/autoCompact.ts` `autoCompactIfNeeded(...)`.
//!
//! Estimates thresholds, invokes the shared model-backed conversation compact
//! path, transports rebuilt Read state, and returns the post-compact message
//! view. Session-memory-first routing remains owned by the unported
//! `sessionMemoryCompact.ts` path.

use crate::constants::query_source::QuerySource;
use crate::tool::ToolUseContext;
use crate::types::message::Message;
use crate::utils::process_env::JsTruthy;
use std::collections::BTreeMap;

pub const MAX_OUTPUT_TOKENS_FOR_SUMMARY: i64 = 20_000;
pub const AUTOCOMPACT_BUFFER_TOKENS: i64 = 13_000;
pub const WARNING_THRESHOLD_BUFFER_TOKENS: i64 = 20_000;
pub const ERROR_THRESHOLD_BUFFER_TOKENS: i64 = 20_000;
pub const MANUAL_COMPACT_BUFFER_TOKENS: i64 = 3_000;
pub const MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenWarningState {
    pub percent_left: i64,
    pub is_above_warning_threshold: bool,
    pub is_above_error_threshold: bool,
    pub is_above_auto_compact_threshold: bool,
    pub is_at_blocking_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoCompactTrackingState {
    /// Maps to CC `AutoCompactTrackingState.compacted`.
    pub compacted: bool,
    /// Maps to CC `AutoCompactTrackingState.turnCounter`.
    pub turn_counter: u32,
    /// Maps to CC `AutoCompactTrackingState.turnId`.
    pub turn_id: String,
    /// Maps to CC `AutoCompactTrackingState.consecutiveFailures`.
    pub consecutive_failures: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoCompactCacheSafeParams {
    /// Maps to CC `CacheSafeParams.systemPrompt`.
    pub system_prompt: Vec<String>,
    /// Maps to CC `CacheSafeParams.userContext`.
    pub user_context: BTreeMap<String, String>,
    /// Maps to CC `CacheSafeParams.systemContext`.
    pub system_context: BTreeMap<String, String>,
    /// Maps to CC `CacheSafeParams.forkContextMessages`.
    pub fork_context_messages: Vec<Message>,
}

#[derive(Debug, Clone, Default)]
pub struct AutocompactResult {
    /// Maps to: CC post-`autoCompactIfNeeded(...)` messages used for the
    /// current model request. On a compact transition this is
    /// `buildPostCompactMessages(compactionResult)` (boundary first), and the
    /// query loop yields every member as a whole `Message`
    /// (CC query.ts:528-535) — no separate boundary carrier exists.
    pub messages: Vec<Message>,
    /// True when this result represents a compact transition.
    pub compacted: bool,
    /// Maps to: CC `consecutiveFailures` circuit-breaker state.
    pub consecutive_failures: Option<u32>,
    /// Rust transport for the mutable `ToolUseContext.readFileState` effect
    /// performed by official post-compact file restoration.
    pub rebuilt_read_file_state: Option<Vec<crate::utils::query_helpers::ReadFileStateEntry>>,
}

/// Maps to CC `services/compact/autoCompact.ts` `getEffectiveContextWindowSize(...)`.
pub fn get_effective_context_window_size(model: &str) -> i64 {
    let reserved_tokens_for_summary =
        (crate::services::api::claude::get_max_output_tokens_for_model(model) as i64)
            .min(MAX_OUTPUT_TOKENS_FOR_SUMMARY);
    let mut context_window = crate::utils::context::get_context_window_for_model(model, &[]);

    if let Some(value) = crate::utils::process_env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW").truthy() {
        if let Ok(parsed) = value.parse::<i64>() {
            if parsed > 0 {
                context_window = context_window.min(parsed);
            }
        }
    }

    context_window - reserved_tokens_for_summary
}

/// Maps to CC `services/compact/autoCompact.ts` `getAutoCompactThreshold(...)`.
pub fn get_auto_compact_threshold(model: &str) -> i64 {
    let effective_context_window = get_effective_context_window_size(model);
    let autocompact_threshold = effective_context_window - AUTOCOMPACT_BUFFER_TOKENS;

    if let Some(value) = crate::utils::process_env::var("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE").truthy() {
        if let Ok(parsed) = value.parse::<f64>() {
            if parsed > 0.0 && parsed <= 100.0 {
                let percentage_threshold =
                    (effective_context_window as f64 * (parsed / 100.0)).floor() as i64;
                return percentage_threshold.min(autocompact_threshold);
            }
        }
    }

    autocompact_threshold
}

/// Maps to CC `services/compact/autoCompact.ts` `isAutoCompactEnabled()`.
pub fn is_auto_compact_enabled() -> bool {
    is_auto_compact_enabled_with_config(&crate::utils::config::load_global_config())
}

pub fn is_auto_compact_enabled_with_config(config: &crate::utils::config::GlobalConfig) -> bool {
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("DISABLE_COMPACT").as_deref())
        || crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("DISABLE_AUTO_COMPACT").as_deref(),
        )
    {
        return false;
    }
    config.auto_compact_enabled.unwrap_or(true)
}

/// Maps to CC `services/compact/autoCompact.ts` `calculateTokenWarningState(...)`.
pub fn calculate_token_warning_state(token_usage: i64, model: &str) -> TokenWarningState {
    calculate_token_warning_state_with_config(
        token_usage,
        model,
        &crate::utils::config::load_global_config(),
    )
}

pub fn calculate_token_warning_state_with_config(
    token_usage: i64,
    model: &str,
    config: &crate::utils::config::GlobalConfig,
) -> TokenWarningState {
    let auto_compact_threshold = get_auto_compact_threshold(model);
    let threshold = if is_auto_compact_enabled_with_config(config) {
        auto_compact_threshold
    } else {
        get_effective_context_window_size(model)
    };

    let percent_left =
        (((threshold - token_usage) as f64 / threshold as f64) * 100.0).round() as i64;
    let percent_left = percent_left.max(0);
    let warning_threshold = threshold - WARNING_THRESHOLD_BUFFER_TOKENS;
    let error_threshold = threshold - ERROR_THRESHOLD_BUFFER_TOKENS;
    let actual_context_window = get_effective_context_window_size(model);
    let default_blocking_limit = actual_context_window - MANUAL_COMPACT_BUFFER_TOKENS;
    let blocking_limit = crate::utils::process_env::var("CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_blocking_limit);

    TokenWarningState {
        percent_left,
        is_above_warning_threshold: token_usage >= warning_threshold,
        is_above_error_threshold: token_usage >= error_threshold,
        is_above_auto_compact_threshold: is_auto_compact_enabled_with_config(config)
            && token_usage >= auto_compact_threshold,
        is_at_blocking_limit: token_usage >= blocking_limit,
    }
}

/// Maps to CC `services/compact/autoCompact.ts` `shouldAutoCompact(...)`.
pub fn should_auto_compact_with_config(
    messages: &[Message],
    model: &str,
    query_source: Option<&str>,
    snip_tokens_freed: i64,
    config: &crate::utils::config::GlobalConfig,
) -> bool {
    if matches!(
        query_source,
        Some("session_memory" | "compact" | "marble_origami")
    ) {
        return false;
    }
    if !is_auto_compact_enabled_with_config(config) {
        return false;
    }
    // Maps to CC `services/compact/autoCompact.ts` `shouldAutoCompact(...)`:
    // when context-collapse is enabled it owns proactive context headroom, so
    // automatic compaction must not race it and discard granular collapse state.
    if crate::services::context_collapse::is_context_collapse_enabled() {
        return false;
    }

    let token_count = crate::utils::tokens::token_count_with_estimation(messages)
        .saturating_sub(snip_tokens_freed);
    calculate_token_warning_state_with_config(token_count, model, config)
        .is_above_auto_compact_threshold
}

/// Maps to: CC `services/compact/autoCompact.ts` `autoCompactIfNeeded(...)`.
pub async fn auto_compact_if_needed(
    messages_for_query: Vec<Message>,
    tool_use_context: &ToolUseContext,
    cache_safe_params: AutoCompactCacheSafeParams,
    query_source: &QuerySource,
    tracking: Option<AutoCompactTrackingState>,
    snip_tokens_freed: i64,
) -> AutocompactResult {
    if crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var("DISABLE_COMPACT").as_deref()) {
        return AutocompactResult {
            messages: messages_for_query,
            compacted: false,
            consecutive_failures: tracking.and_then(|state| state.consecutive_failures),
            rebuilt_read_file_state: None,
        };
    }

    let consecutive_failures = tracking.and_then(|state| state.consecutive_failures);
    if consecutive_failures.is_some_and(|failures| failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES)
    {
        return AutocompactResult {
            messages: messages_for_query,
            compacted: false,
            consecutive_failures,
            rebuilt_read_file_state: None,
        };
    }

    let model = tool_use_context
        .main_loop_model
        .clone()
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let config = crate::utils::config::load_global_config();
    let should_attempt = should_auto_compact_with_config(
        &messages_for_query,
        &model,
        Some(query_source.as_api_source()),
        snip_tokens_freed,
        &config,
    );

    if should_attempt {
        let original_messages = messages_for_query.clone();
        return match crate::services::compact::compact::compact_conversation(
            messages_for_query,
            tool_use_context,
            &cache_safe_params,
            true,
            None,
            true,
        )
        .await
        {
            Ok(compaction_result) => {
                crate::services::compact::post_compact_cleanup::run_post_compact_cleanup(Some(
                    query_source,
                ));
                AutocompactResult {
                    // CC query.ts:528-535: `buildPostCompactMessages(...)` IS
                    // the yield set — its first member is the real boundary
                    // marker (`compaction_result.boundary_marker`); the old
                    // freshly-minted duplicate boundary Row is gone.
                    messages: crate::services::compact::compact::build_post_compact_messages(
                        &compaction_result,
                    ),
                    compacted: true,
                    consecutive_failures: Some(0),
                    rebuilt_read_file_state: Some(
                        compaction_result.rebuilt_read_file_state.clone(),
                    ),
                }
            }
            Err(_error) => {
                // Preserve CC's failure circuit-breaker position: only failed
                // attempted compactions increment this count.
                let next_failures = consecutive_failures.unwrap_or(0).saturating_add(1);
                AutocompactResult {
                    messages: original_messages,
                    compacted: false,
                    consecutive_failures: Some(next_failures),
                    rebuilt_read_file_state: None,
                }
            }
        };
    }

    AutocompactResult {
        messages: messages_for_query,
        compacted: false,
        consecutive_failures,
        rebuilt_read_file_state: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn clear_context_collapse_env() {
        crate::utils::process_env::remove("COMETIX_CONTEXT_COLLAPSE");
        crate::utils::process_env::remove("CLAUDE_CODE_CONTEXT_COLLAPSE");
        crate::utils::process_env::remove("CLAUDE_CONTEXT_COLLAPSE");
    }

    #[test]
    fn auto_compact_threshold_helpers_match_official_buffers_and_env_overrides() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
        crate::utils::process_env::remove("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");

        assert_eq!(
            get_effective_context_window_size("claude-sonnet-4-20250514"),
            180_000
        );
        assert_eq!(
            get_auto_compact_threshold("claude-sonnet-4-20250514"),
            167_000
        );

        crate::utils::process_env::set("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", "50");
        assert_eq!(
            get_auto_compact_threshold("claude-sonnet-4-20250514"),
            90_000
        );
        crate::utils::process_env::remove("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE");
    }

    #[test]
    fn token_warning_state_respects_auto_compact_config_and_blocking_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_COMPACT");
        crate::utils::process_env::remove("DISABLE_AUTO_COMPACT");
        crate::utils::process_env::remove("CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE");
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        let mut config = crate::utils::config::GlobalConfig::default();
        config.auto_compact_enabled = Some(false);

        let state =
            calculate_token_warning_state_with_config(170_000, "claude-sonnet-4-20250514", &config);
        assert!(!state.is_above_auto_compact_threshold);
        assert!(state.is_above_warning_threshold);
        assert!(!state.is_at_blocking_limit);

        crate::utils::process_env::set("CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE", "169000");
        let state =
            calculate_token_warning_state_with_config(170_000, "claude-sonnet-4-20250514", &config);
        assert!(state.is_at_blocking_limit);

        let state = calculate_token_warning_state_with_config(
            -10_000,
            "claude-sonnet-4-20250514",
            &crate::utils::config::GlobalConfig::default(),
        );
        assert!(state.percent_left > 100);
        crate::utils::process_env::remove("CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE");
    }

    #[test]
    fn should_auto_compact_uses_estimated_tokens_and_official_recursion_guards() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_COMPACT");
        crate::utils::process_env::remove("DISABLE_AUTO_COMPACT");
        clear_context_collapse_env();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");
        crate::utils::process_env::remove("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        let config = crate::utils::config::GlobalConfig::default();
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text("x".repeat(90_000))],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];

        assert!(should_auto_compact_with_config(
            &messages,
            "claude-sonnet-4-20250514",
            Some("repl_main_thread"),
            0,
            &config,
        ));
        for guarded_source in ["compact", "session_memory", "marble_origami"] {
            assert!(!should_auto_compact_with_config(
                &messages,
                "claude-sonnet-4-20250514",
                Some(guarded_source),
                0,
                &config,
            ));
        }

        crate::utils::process_env::set("CLAUDE_CONTEXT_COLLAPSE", "1");
        assert!(!should_auto_compact_with_config(
            &messages,
            "claude-sonnet-4-20250514",
            Some("repl_main_thread"),
            0,
            &config,
        ));
        clear_context_collapse_env();
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    #[tokio::test]
    async fn auto_compact_if_needed_records_failed_attempt_when_threshold_would_compact() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_COMPACT");
        crate::utils::process_env::remove("DISABLE_AUTO_COMPACT");
        clear_context_collapse_env();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");
        crate::utils::process_env::remove("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text("x".repeat(90_000))],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let context = ToolUseContext::default();
        context.abort_controller.abort();
        let params = AutoCompactCacheSafeParams {
            fork_context_messages: messages.clone(),
            ..AutoCompactCacheSafeParams::default()
        };

        let result = auto_compact_if_needed(
            messages.clone(),
            &context,
            params,
            &QuerySource::Prompt,
            Some(AutoCompactTrackingState {
                compacted: false,
                turn_counter: 0,
                turn_id: "turn-auto".to_string(),
                consecutive_failures: Some(2),
            }),
            0,
        )
        .await;

        assert_eq!(result.messages, messages);
        assert!(!result.compacted);
        assert_eq!(result.consecutive_failures, Some(3));
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    #[tokio::test]
    async fn auto_compact_if_needed_does_not_fake_success_when_summary_is_aborted() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("DISABLE_COMPACT");
        crate::utils::process_env::remove("DISABLE_AUTO_COMPACT");
        clear_context_collapse_env();
        crate::utils::process_env::set("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "50000");
        crate::utils::process_env::remove("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_OUTPUT_TOKENS");
        crate::utils::process_env::remove("COMETIX_MAX_TOKENS_CAP");
        crate::utils::process_env::remove("CLAUDE_CODE_MAX_CONTEXT_TOKENS");
        crate::utils::process_env::remove("CLAUDE_CODE_DISABLE_1M_CONTEXT");
        let messages = (0..6)
            .map(|i| {
                Message::User(crate::types::message::UserMessage {
                    uuid: uuid::Uuid::new_v4().to_string(),
                    timestamp: chrono::Utc::now(),
                    content: vec![crate::types::message::UserContent::Text(format!(
                        "message {i} {}",
                        "x".repeat(15_000)
                    ))],
                    is_compact_summary: false,
                    plan_content: None,
                    image_paste_ids: None,
                    is_visible_in_transcript_only: false,
                    mcp_meta: None,
                    source_tool_assistant_uuid: None,
                    permission_mode: None,
                    origin: None,
                    summarize_metadata: None,
                })
            })
            .collect::<Vec<_>>();
        let context = ToolUseContext::default();
        context.abort_controller.abort();
        let params = AutoCompactCacheSafeParams {
            fork_context_messages: messages.clone(),
            ..AutoCompactCacheSafeParams::default()
        };

        let result = auto_compact_if_needed(
            messages.clone(),
            &context,
            params,
            &QuerySource::Prompt,
            Some(AutoCompactTrackingState {
                compacted: false,
                turn_counter: 0,
                turn_id: "turn-auto".to_string(),
                consecutive_failures: Some(2),
            }),
            0,
        )
        .await;

        assert!(!result.compacted);
        assert_eq!(result.messages, messages);
        assert_eq!(result.consecutive_failures, Some(3));
        crate::utils::process_env::remove("CLAUDE_CODE_AUTO_COMPACT_WINDOW");
    }

    #[tokio::test]
    async fn auto_compact_if_needed_noops_when_below_threshold() {
        let messages = vec![Message::User(crate::types::message::UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![crate::types::message::UserContent::Text(
                "before autocompact".to_string(),
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
        })];
        let context = ToolUseContext::default();
        let params = AutoCompactCacheSafeParams {
            fork_context_messages: messages.clone(),
            ..AutoCompactCacheSafeParams::default()
        };
        let tracking = AutoCompactTrackingState {
            compacted: false,
            turn_counter: 2,
            turn_id: "turn-auto".to_string(),
            consecutive_failures: Some(1),
        };

        let result = auto_compact_if_needed(
            messages.clone(),
            &context,
            params,
            &QuerySource::Prompt,
            Some(tracking),
            123,
        )
        .await;

        assert_eq!(result.messages, messages);
        assert!(!result.compacted);
        assert_eq!(result.consecutive_failures, Some(1));
    }
}
