//! @nyan prompt-cache warming.
//!
//! Warming is deliberately a cache-sharing fork: it replays the last real
//! request prefix, sends one user token, never writes a transcript, and never
//! makes a content-replacement decision.

use crate::constants::query_source::QuerySource;
use crate::utils::forked_agent::{
    CacheSafeParams, ForkedAgentParams, get_last_cache_safe_params, is_cache_sharing_fork,
    run_forked_agent,
};
use crate::utils::settings::nyan::CacheWarmingMode;
use crate::utils::thinking::{
    ThinkingConfig, model_supports_adaptive_thinking, model_supports_thinking,
};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct WarmerState {
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
}

static STATE: LazyLock<Mutex<WarmerState>> = LazyLock::new(|| Mutex::new(WarmerState::default()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmEligibility {
    pub ttl: Duration,
    pub cached_tokens: u64,
}

fn cache_settings() -> crate::utils::settings::nyan::CacheSettings {
    crate::utils::settings::get_initial_settings()
        .nyan
        .map(|settings| settings.cache())
        .unwrap_or_default()
}

/// Read the final in-memory usage, not a transcript usage snapshot. `profile`
/// is the one the request went through (`None`: the main profile).
pub fn warm_eligibility(
    usage: &crate::types::message::TokenUsage,
    model: &str,
    profile: Option<&str>,
    thinking: &ThinkingConfig,
    has_tool_use: bool,
) -> Option<WarmEligibility> {
    let Some(ttl_usage) =
        crate::services::provider::anthropic::cache::last_cache_ttl_usage_for(model)
    else {
        // No recorded TTL for this model (gateway auto-cache, or a model switch
        // since the last recorded write): do not pretend a refresh helps.
        return None;
    };
    warm_eligibility_with_ttl(usage, ttl_usage, model, profile, thinking, has_tool_use)
}

pub fn warm_eligibility_with_ttl(
    usage: &crate::types::message::TokenUsage,
    ttl_usage: crate::services::provider::anthropic::cache::CacheTtlUsage,
    model: &str,
    profile: Option<&str>,
    thinking: &ThinkingConfig,
    has_tool_use: bool,
) -> Option<WarmEligibility> {
    warm_eligibility_with_settings(
        &cache_settings(),
        usage,
        ttl_usage,
        model,
        profile,
        thinking,
        has_tool_use,
    )
}

fn warm_eligibility_with_settings(
    settings: &crate::utils::settings::nyan::CacheSettings,
    usage: &crate::types::message::TokenUsage,
    ttl_usage: crate::services::provider::anthropic::cache::CacheTtlUsage,
    model: &str,
    profile: Option<&str>,
    thinking: &ThinkingConfig,
    has_tool_use: bool,
) -> Option<WarmEligibility> {
    let mode = settings.warming();
    if mode == CacheWarmingMode::Off || (!has_tool_use && mode == CacheWarmingMode::Active) {
        return None;
    }
    // `max_tokens = 1` is only a legal, prefix-identical request when thinking
    // is effectively off. `Enabled`, or an `Adaptive` that the request builder
    // would convert to `Enabled`, would send a different thinking budget.
    let thinking_is_neutral = !model_supports_thinking(model, profile)
        || matches!(thinking, ThinkingConfig::Disabled)
        || matches!(
            thinking,
            ThinkingConfig::Adaptive if model_supports_adaptive_thinking(model, profile)
        );
    if !thinking_is_neutral {
        return None;
    }
    let cached_tokens = usage
        .cache_read_input_tokens
        .saturating_add(usage.cache_creation_input_tokens);
    if cached_tokens < u64::from(settings.min_cached_tokens()) {
        return None;
    }
    let ttl = if ttl_usage.ephemeral_1h_input_tokens > 0 {
        Duration::from_secs(60 * 60)
    } else if ttl_usage.ephemeral_5m_input_tokens > 0 {
        Duration::from_secs(5 * 60)
    } else {
        // Gateway-managed cache TTL is unknown; do not pretend a refresh is
        // useful when the response did not advertise an ephemeral TTL.
        return None;
    };
    Some(WarmEligibility { ttl, cached_tokens })
}

fn current_generation(generation: u64) -> bool {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .generation
        == generation
}

/// Stop a pending warmer because the main request prefix may have changed.
pub fn invalidate() {
    let mut state = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.generation = state.generation.wrapping_add(1);
    if let Some(task) = state.task.take() {
        task.abort();
    }
}

/// Only main-thread requests own the cached prefix. Cache-sharing forks
/// (including the warmer itself) and sub-agents must not stop the warmer.
pub fn invalidate_for_source(source: &QuerySource) {
    if matches!(source, QuerySource::Prompt | QuerySource::Sdk) {
        invalidate();
    }
}

fn deny_tools() -> crate::tool::CanUseToolCallback {
    crate::tool::CanUseToolCallback::new(
        |_tool, _input, _context, _assistant, _tool_use_id, _force| {
            crate::types::permissions::PermissionDecision::Deny {
                message: "Tool use is not allowed during cache warming".to_string(),
                decision_reason: crate::types::permissions::PermissionDecisionReason::Other {
                    reason: "cache warmer is text-only".to_string(),
                },
                tool_use_id: None,
            }
        },
    )
}

fn warming_message() -> crate::types::message::Message {
    crate::types::message::Message::User(crate::types::message::UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![crate::types::message::UserContent::MetaText(
            "Continue.".to_string(),
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
    })
}

/// The keep-alive request is accounted at the request layer like any other.
async fn run_one(cache: &CacheSafeParams) -> anyhow::Result<()> {
    let result = run_forked_agent(ForkedAgentParams {
        prompt_messages: vec![warming_message()],
        cache_safe_params: cache.clone(),
        can_use_tool: deny_tools(),
        query_source: QuerySource::CacheWarm,
        fork_label: "cache_warm".to_string(),
        overrides: None,
        max_output_tokens: Some(1),
        max_turns: Some(1),
        on_message: None,
        on_progress: None,
        skip_cache_write: false,
        skip_transcript: true,
    })
    .await?;
    Ok(())
}

/// Entry point shared by both lifecycle sites: the query loop (a response that
/// asks for tools, so the agent keeps running) and the stop hook (the turn
/// ended, so the agent is waiting for the user). `cache` must be the prefix the
/// request was actually sent with; the final in-memory usage comes from the
/// last assistant message, never from the transcript.
pub fn observe_response(
    source: &QuerySource,
    cache: Arc<CacheSafeParams>,
    assistant_messages: &[crate::types::message::AssistantMessage],
) {
    let Some(last) = assistant_messages.last() else {
        return;
    };
    let Some(usage) = last.usage.as_ref() else {
        return;
    };
    let context = &cache.tool_use_context;
    // The TTL record is keyed by the request model (`Options.model`), so read
    // the same source here rather than the response echo.
    let model = context
        .main_loop_model
        .clone()
        .or_else(|| last.model.clone())
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let profile = context.main_loop_profile.clone();
    let thinking = context.thinking_config.clone().unwrap_or_else(|| {
        crate::utils::thinking::production_thinking_config_from_env_and_settings(
            &crate::utils::settings::get_initial_settings(),
        )
    });
    // Only the response being observed decides whether the agent continues;
    // earlier tool rounds in the same turn must not keep "active" warming alive
    // after the turn ended.
    let has_tool_use = last
        .content
        .iter()
        .any(|content| matches!(content, crate::types::message::AssistantContent::ToolUse(_)));
    observe_real_request(
        source,
        Some(cache),
        usage,
        &model,
        profile.as_deref(),
        &thinking,
        has_tool_use,
    );
}

/// Observe one completed real request and schedule a bounded refresh if the
/// response advertised a cache TTL. Calling this again invalidates the old
/// task, which makes new messages, model changes, masks, and summaries stop the
/// previous warmer immediately.
pub fn observe_real_request(
    source: &QuerySource,
    cache: Option<Arc<CacheSafeParams>>,
    usage: &crate::types::message::TokenUsage,
    model: &str,
    profile: Option<&str>,
    thinking: &ThinkingConfig,
    has_tool_use: bool,
) {
    if is_cache_sharing_fork(source) || !matches!(source, QuerySource::Prompt | QuerySource::Sdk) {
        return;
    }
    invalidate();
    let Some(eligibility) = warm_eligibility(usage, model, profile, thinking, has_tool_use) else {
        return;
    };
    let Some(cache) = cache.or_else(get_last_cache_safe_params) else {
        return;
    };
    let settings = cache_settings();
    let max_minutes = if has_tool_use {
        settings.max_active_minutes()
    } else {
        settings.max_idle_minutes()
    };
    if max_minutes == 0 {
        return;
    }
    let margin = std::cmp::max(Duration::from_secs(10), eligibility.ttl / 10);
    let delay = eligibility.ttl.saturating_sub(margin);
    let generation = {
        let state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation
    };
    let started = Instant::now();
    let max_duration = Duration::from_secs(u64::from(max_minutes) * 60);
    let task = tokio::spawn(async move {
        let mut next_delay = delay;
        loop {
            let tick = Instant::now();
            tokio::time::sleep(next_delay).await;
            if !current_generation(generation) || started.elapsed() >= max_duration {
                return;
            }
            // If the scheduler was suspended beyond half the safety margin,
            // refreshing now is equivalent to paying for a cold rewrite.
            if tick.elapsed() > next_delay + margin / 2 {
                return;
            }
            match run_one(&cache).await {
                Ok(()) => {
                    next_delay = eligibility.ttl.saturating_sub(margin);
                }
                Err(_) => return,
            }
        }
    });
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .task = Some(task);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::anthropic::cache::CacheTtlUsage;
    use crate::utils::settings::nyan::CacheSettings;

    fn usage(cached: u64) -> crate::types::message::TokenUsage {
        crate::types::message::TokenUsage {
            cache_read_input_tokens: cached,
            ..Default::default()
        }
    }

    fn ttl_5m() -> CacheTtlUsage {
        CacheTtlUsage {
            ephemeral_5m_input_tokens: 25_000,
            ..Default::default()
        }
    }

    fn settings(warming: CacheWarmingMode) -> CacheSettings {
        CacheSettings {
            warming: Some(warming),
            ..Default::default()
        }
    }

    fn eligible(
        settings: &CacheSettings,
        usage: &crate::types::message::TokenUsage,
        ttl: CacheTtlUsage,
        model: &str,
        thinking: &ThinkingConfig,
        has_tool_use: bool,
    ) -> Option<WarmEligibility> {
        warm_eligibility_with_settings(settings, usage, ttl, model, None, thinking, has_tool_use)
    }

    #[test]
    fn ttl_and_token_floor_come_from_the_final_in_memory_usage() {
        let on = settings(CacheWarmingMode::Active);
        let five = eligible(
            &on,
            &usage(25_000),
            ttl_5m(),
            "claude-sonnet-4-6",
            &ThinkingConfig::Disabled,
            true,
        )
        .expect("5m cache is eligible");
        assert_eq!(five.ttl, Duration::from_secs(300));
        assert_eq!(five.cached_tokens, 25_000);

        let hour = CacheTtlUsage {
            ephemeral_1h_input_tokens: 25_000,
            ..Default::default()
        };
        let one_hour = eligible(
            &on,
            &usage(25_000),
            hour,
            "claude-sonnet-4-6",
            &ThinkingConfig::Disabled,
            true,
        )
        .expect("1h cache is eligible");
        assert_eq!(one_hour.ttl, Duration::from_secs(3600));

        // cache_read + cache_creation both count toward the floor.
        let split = crate::types::message::TokenUsage {
            cache_read_input_tokens: 12_000,
            cache_creation_input_tokens: 9_000,
            ..Default::default()
        };
        assert!(
            eligible(
                &on,
                &split,
                ttl_5m(),
                "claude-sonnet-4-6",
                &ThinkingConfig::Disabled,
                true
            )
            .is_some()
        );
        assert!(
            eligible(
                &on,
                &usage(19_999),
                ttl_5m(),
                "claude-sonnet-4-6",
                &ThinkingConfig::Disabled,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn unknown_ttl_never_warms() {
        let on = settings(CacheWarmingMode::Active);
        // Gateway models report cache_read but no ephemeral_* breakdown.
        assert!(
            eligible(
                &on,
                &usage(90_000),
                CacheTtlUsage::default(),
                "gpt-6.1-sol",
                &ThinkingConfig::Disabled,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn warming_mode_gates_active_idle_and_off() {
        let args = |settings: &CacheSettings, has_tool_use: bool| {
            eligible(
                settings,
                &usage(25_000),
                ttl_5m(),
                "claude-sonnet-4-6",
                &ThinkingConfig::Disabled,
                has_tool_use,
            )
        };
        assert!(args(&settings(CacheWarmingMode::Off), true).is_none());
        assert!(args(&settings(CacheWarmingMode::Off), false).is_none());
        // Active: only while the agent still has tools to run.
        assert!(args(&settings(CacheWarmingMode::Active), true).is_some());
        assert!(args(&settings(CacheWarmingMode::Active), false).is_none());
        // Idle also covers the wait for user input.
        assert!(args(&settings(CacheWarmingMode::Idle), true).is_some());
        assert!(args(&settings(CacheWarmingMode::Idle), false).is_some());
        // Unset defaults to active.
        assert!(args(&CacheSettings::default(), true).is_some());
        assert!(args(&CacheSettings::default(), false).is_none());
    }

    #[test]
    fn enabled_and_converted_adaptive_thinking_never_warms() {
        let on = settings(CacheWarmingMode::Active);
        let enabled = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        assert!(!model_supports_adaptive_thinking("claude-sonnet-4-5", None));
        assert!(model_supports_thinking("claude-sonnet-4-5", None));
        assert!(
            eligible(
                &on,
                &usage(25_000),
                ttl_5m(),
                "claude-sonnet-4-5",
                &enabled,
                true
            )
            .is_none()
        );
        // Adaptive on a model without native adaptive thinking is rewritten to
        // Enabled by the request builder, so it is not prefix-identical either.
        assert!(
            eligible(
                &on,
                &usage(25_000),
                ttl_5m(),
                "claude-sonnet-4-5",
                &ThinkingConfig::Adaptive,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn disabled_or_native_adaptive_thinking_may_warm() {
        let on = settings(CacheWarmingMode::Active);
        assert!(
            eligible(
                &on,
                &usage(25_000),
                ttl_5m(),
                "claude-sonnet-4-5",
                &ThinkingConfig::Disabled,
                true
            )
            .is_some()
        );
        let adaptive_model = "claude-sonnet-4-6";
        assert!(model_supports_adaptive_thinking(adaptive_model, None));
        assert!(
            eligible(
                &on,
                &usage(25_000),
                ttl_5m(),
                adaptive_model,
                &ThinkingConfig::Adaptive,
                true
            )
            .is_some()
        );
    }

    #[test]
    fn adaptive_5_5_models_may_warm_unless_their_profile_declares_a_budget() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        struct ResetSettings;
        impl Drop for ResetSettings {
            fn drop(&mut self) {
                crate::utils::settings::settings_cache::reset_settings_cache();
            }
        }
        let _reset = ResetSettings;
        let on = settings(CacheWarmingMode::Active);
        let adaptive = |model: &str, profile: Option<&str>| {
            warm_eligibility_with_settings(
                &on,
                &usage(25_000),
                ttl_5m(),
                model,
                profile,
                &ThinkingConfig::Adaptive,
                true,
            )
        };
        assert!(adaptive("claude-opus-5-5", None).is_some());
        assert!(adaptive("claude-sonnet-5-5", None).is_some());
        assert!(adaptive("claude-haiku-5-5", None).is_some());
        assert!(adaptive("claude-haiku-4-5-20251001", None).is_none());

        crate::utils::settings::settings_cache::install_test_settings(
            serde_json::from_value(serde_json::json!({"nyan": {
                "providers": {"p": {"kind": "anthropic", "protocol": "anthropicMessages"}},
                "models": {"main": {"provider": "p", "model": "claude-opus-5-5",
                    "capabilities": {"contextWindow": 200000, "maxOutputTokens": 64000,
                        "thinking": "budgeted"}}},
                "selection": {"main": "main"}
            }}))
            .unwrap(),
        );
        // The request builder would send a budget, so `max_tokens = 1` is not
        // prefix-identical.
        assert!(adaptive("claude-opus-5-5", Some("main")).is_none());
        assert!(adaptive("claude-opus-5-5", None).is_none());
    }

    #[test]
    fn only_main_thread_and_non_fork_sources_may_invalidate() {
        invalidate();
        let before = STATE.lock().unwrap().generation;
        for source in [
            QuerySource::CacheWarm,
            QuerySource::Compact,
            QuerySource::SideQuestion,
            QuerySource::AgentCustom,
        ] {
            invalidate_for_source(&source);
        }
        assert_eq!(STATE.lock().unwrap().generation, before);
        invalidate_for_source(&QuerySource::Prompt);
        assert_eq!(STATE.lock().unwrap().generation, before.wrapping_add(1));
    }

    #[test]
    fn thinking_neutrality_covers_models_without_thinking() {
        // A model that supports no thinking at all sends no thinking block, so
        // `max_tokens = 1` stays prefix-identical even if settings asked for
        // thinking.
        let on = settings(CacheWarmingMode::Active);
        let model = "claude-3-5-haiku-20241022";
        assert!(!model_supports_thinking(model, None));
        assert!(
            eligible(
                &on,
                &usage(25_000),
                ttl_5m(),
                model,
                &ThinkingConfig::Enabled {
                    budget_tokens: Some(4096)
                },
                true,
            )
            .is_some()
        );
    }

    #[test]
    fn sdk_main_thread_also_invalidates() {
        invalidate();
        let before = STATE.lock().unwrap().generation;
        invalidate_for_source(&QuerySource::Sdk);
        assert_eq!(STATE.lock().unwrap().generation, before.wrapping_add(1));
    }
}
