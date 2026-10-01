//! Model-backed prompt suggestion generation.
//! Maps to CC `services/PromptSuggestion/promptSuggestion.ts`.

use crate::state::app_state_store::AppState;
use crate::tool::{AbortController, CanUseToolCallback};
use crate::types::message::{AssistantContent, Message, UserContent, UserMessage};
use crate::types::permissions::{PermissionDecision, PermissionDecisionReason};
use crate::utils::forked_agent::{CacheSafeParams, ForkedAgentParams, SubagentContextOverrides};
use std::sync::{LazyLock, Mutex};

const MAX_PARENT_UNCACHED_TOKENS: u64 = 10_000;

const SUGGESTION_PROMPT: &str = r#"[SUGGESTION MODE: Suggest what the user might naturally type next into Claude Code.]

FIRST: Look at the user's recent messages and original request.

Your job is to predict what THEY would type - not what you think they should do.

THE TEST: Would they think "I was just about to type that"?

EXAMPLES:
User asked "fix the bug and run tests", bug is fixed → "run the tests"
After code written → "try it out"
Claude offers options → suggest the one the user would likely pick, based on conversation
Claude asks to continue → "yes" or "go ahead"
Task complete, obvious follow-up → "commit this" or "push it"
After error or misunderstanding → silence (let them assess/correct)

Be specific: "run the tests" beats "continue".

NEVER SUGGEST:
- Evaluative ("looks good", "thanks")
- Questions ("what about...?")
- Claude-voice ("Let me...", "I'll...", "Here's...")
- New ideas they didn't ask about
- Multiple sentences

Stay silent if the next step isn't obvious from what the user said.

Format: 2-12 words, match the user's style. Or nothing.

Reply with ONLY the suggestion, no quotes or explanation."#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptVariant {
    UserIntent,
    StatedIntent,
}

impl PromptVariant {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::UserIntent => "user_intent",
            Self::StatedIntent => "stated_intent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedSuggestion {
    pub suggestion: String,
    pub prompt_id: PromptVariant,
    pub generation_request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionGeneration {
    pub suggestion: Option<String>,
    pub generation_request_id: Option<String>,
}

static CURRENT_ABORT_CONTROLLER: LazyLock<Mutex<Option<AbortController>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn get_prompt_variant() -> PromptVariant {
    PromptVariant::UserIntent
}

pub fn should_enable_prompt_suggestion_with_snapshots(
    setting: Option<bool>,
    non_interactive: bool,
    swarm_teammate: bool,
    env_override: Option<&str>,
) -> bool {
    if crate::utils::env_utils::is_env_defined_falsy(env_override) {
        return false;
    }
    if crate::utils::env_utils::is_env_truthy(env_override) {
        return true;
    }
    // Maps to CC `getFeatureValue_CACHED_MAY_BE_STALE('tengu_chomp_inflection', false)`,
    // resolved from the source-controlled switch table instead of GrowthBook.
    let cohort_enabled = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::PromptSuggestion,
    );
    cohort_enabled && !non_interactive && !swarm_teammate && setting.unwrap_or(true)
}

/// Maps to CC `shouldEnablePromptSuggestion()` without telemetry.
pub fn should_enable_prompt_suggestion() -> bool {
    let settings = crate::utils::settings::get_initial_settings();
    let swarm_teammate = crate::utils::agent_swarms_enabled::is_agent_swarms_enabled()
        && crate::utils::teammate::is_teammate();
    should_enable_prompt_suggestion_with_snapshots(
        settings.prompt_suggestion_enabled,
        crate::bootstrap::state::get_is_non_interactive_session(),
        swarm_teammate,
        crate::utils::process_env::var("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION")
            .as_deref(),
    )
}

pub fn abort_prompt_suggestion() {
    if let Some(abort) = CURRENT_ABORT_CONTROLLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        abort.abort();
    }
}

/// Pure snapshot form of CC `getSuggestionSuppressReason(...)`.
pub fn get_suggestion_suppress_reason_with_limits(
    app_state: &AppState,
    audience: crate::utils::build_profile::BuildAudience,
    limits: &crate::services::claude_ai_limits::ClaudeAiLimits,
) -> Option<&'static str> {
    if !app_state.prompt_suggestion_enabled {
        return Some("disabled");
    }
    if app_state.pending_worker_request.is_some() || app_state.pending_sandbox_request.is_some() {
        return Some("pending_permission");
    }
    if !app_state.elicitation.queue.is_empty() {
        return Some("elicitation_active");
    }
    if app_state.tool_permission_context.mode == crate::types::permissions::PermissionMode::Plan {
        return Some("plan_mode");
    }
    if !crate::utils::build_profile::audience_has_internal_capability(
        audience,
        crate::utils::build_profile::InternalCapability::Speculation,
    ) && limits.status != crate::services::claude_ai_limits::QuotaStatus::Allowed
    {
        return Some("rate_limit");
    }
    None
}

/// Maps to CC `getSuggestionSuppressReason(...)` using live service state.
pub fn get_suggestion_suppress_reason(app_state: &AppState) -> Option<&'static str> {
    get_suggestion_suppress_reason_with_limits(
        app_state,
        crate::utils::build_profile::build_audience(),
        &crate::services::claude_ai_limits::current_limits(),
    )
}

pub fn get_parent_cache_suppress_reason(
    last_assistant: Option<&crate::types::message::AssistantMessage>,
) -> Option<&'static str> {
    let usage = last_assistant?.usage.as_ref()?;
    let uncached = usage
        .input_tokens
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.output_tokens);
    (uncached > MAX_PARENT_UNCACHED_TOKENS).then_some("cache_cold")
}

fn last_response_is_api_error(messages: &[Message]) -> bool {
    let last_assistant_index = messages
        .iter()
        .rposition(|message| matches!(message, Message::Assistant(_)));
    messages.iter().enumerate().any(|(index, message)| {
        last_assistant_index.is_none_or(|assistant_index| index >= assistant_index)
            && matches!(
                message,
                Message::System(crate::types::message::SystemMessage::ApiError { .. })
            )
    })
}

fn last_assistant(messages: &[Message]) -> Option<&crate::types::message::AssistantMessage> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => Some(assistant),
        _ => None,
    })
}

pub async fn try_generate_suggestion(
    abort_controller: AbortController,
    messages: &[Message],
    cache_safe_params: CacheSafeParams,
) -> anyhow::Result<Option<GeneratedSuggestion>> {
    try_generate_suggestion_with_deps(
        abort_controller,
        messages,
        cache_safe_params,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn try_generate_suggestion_with_deps<D>(
    abort_controller: AbortController,
    messages: &[Message],
    cache_safe_params: CacheSafeParams,
    deps: D,
) -> anyhow::Result<Option<GeneratedSuggestion>>
where
    D: crate::query::deps::QueryDeps,
{
    if abort_controller.is_aborted() {
        return Ok(None);
    }
    if messages
        .iter()
        .filter(|message| matches!(message, Message::Assistant(_)))
        .count()
        < 2
    {
        return Ok(None);
    }
    if last_response_is_api_error(messages) {
        return Ok(None);
    }
    let last_assistant = last_assistant(messages);
    if get_parent_cache_suppress_reason(last_assistant).is_some() {
        return Ok(None);
    }
    let app_state = cache_safe_params.tool_use_context.get_app_state();
    let Some(app_state) = app_state else {
        return Ok(None);
    };
    if get_suggestion_suppress_reason(&app_state).is_some() {
        return Ok(None);
    }

    let prompt_id = get_prompt_variant();
    let generated =
        generate_suggestion_with_deps(abort_controller.clone(), prompt_id, cache_safe_params, deps)
            .await?;
    if abort_controller.is_aborted() {
        return Ok(None);
    }
    let Some(suggestion) = generated.suggestion else {
        return Ok(None);
    };
    if should_filter_suggestion(Some(&suggestion), prompt_id) {
        return Ok(None);
    }
    Ok(Some(GeneratedSuggestion {
        suggestion,
        prompt_id,
        generation_request_id: generated.generation_request_id,
    }))
}

pub async fn generate_suggestion(
    abort_controller: AbortController,
    prompt_id: PromptVariant,
    cache_safe_params: CacheSafeParams,
) -> anyhow::Result<SuggestionGeneration> {
    generate_suggestion_with_deps(
        abort_controller,
        prompt_id,
        cache_safe_params,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn generate_suggestion_with_deps<D>(
    abort_controller: AbortController,
    _prompt_id: PromptVariant,
    cache_safe_params: CacheSafeParams,
    deps: D,
) -> anyhow::Result<SuggestionGeneration>
where
    D: crate::query::deps::QueryDeps,
{
    // Maps to: CC `services/promptSuggestion/promptSuggestion.ts:301-306` —
    // "Deny tools via callback, NOT by passing tools:[]"; `{ behavior: 'deny',
    // message: 'No tools needed for suggestion', decisionReason: { type:
    // 'other', reason: 'suggestion only' } }`.
    let can_use_tool = CanUseToolCallback::new(
        |_tool, _input, _context, _assistant, _tool_use_id, _force| PermissionDecision::Deny {
            message: "No tools needed for suggestion".to_string(),
            decision_reason: PermissionDecisionReason::Other {
                reason: "suggestion only".to_string(),
            },
            tool_use_id: None,
        },
    );
    let result = crate::utils::forked_agent::run_forked_agent_with_deps(
        ForkedAgentParams {
            prompt_messages: vec![Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::Text(SUGGESTION_PROMPT.to_string())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            })],
            cache_safe_params,
            can_use_tool,
            query_source: crate::constants::query_source::QuerySource::PromptSuggestion,
            fork_label: "prompt_suggestion".to_string(),
            overrides: Some(SubagentContextOverrides {
                abort_controller: Some(abort_controller),
                // Suggestion forks must never execute a tool, even when a
                // user hook tries to force an allow decision.
                require_can_use_tool: Some(true),
                ..Default::default()
            }),
            max_output_tokens: None,
            max_turns: None,
            on_message: None,
            on_progress: None,
            skip_transcript: true,
            skip_cache_write: true,
        },
        deps,
    )
    .await?;

    // The assistant envelope is canonical. The aggregate request-id list is
    // retained for compatibility with non-message stream consumers only.
    let generation_request_id = result.messages.iter().find_map(|message| match message {
        Message::Assistant(assistant) => assistant.request_id().map(ToString::to_string),
        _ => None,
    });
    for message in result.messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        let first_text = assistant.content.into_iter().find_map(|block| match block {
            AssistantContent::Text(text) => Some(text),
            _ => None,
        });
        if let Some(text) = first_text {
            let suggestion = text.trim();
            if !suggestion.is_empty() {
                return Ok(SuggestionGeneration {
                    suggestion: Some(suggestion.to_string()),
                    generation_request_id,
                });
            }
        }
    }
    Ok(SuggestionGeneration {
        suggestion: None,
        generation_request_id,
    })
}

/// Maps to CC `shouldFilterSuggestion(...)`; telemetry is intentionally omitted.
pub fn should_filter_suggestion(suggestion: Option<&str>, _prompt_id: PromptVariant) -> bool {
    let Some(suggestion) = suggestion.filter(|suggestion| !suggestion.is_empty()) else {
        return true;
    };
    let lower = suggestion.to_lowercase();
    let words = suggestion.split_whitespace().count();
    if lower == "done"
        || matches!(lower.as_str(), "nothing found" | "nothing found.")
        || lower.starts_with("nothing to suggest")
        || lower.starts_with("no suggestion")
        || META_SILENCE.is_match(&lower)
        || BARE_SILENCE.is_match(&lower)
        || ((suggestion.starts_with('(') && suggestion.ends_with(')'))
            || (suggestion.starts_with('[') && suggestion.ends_with(']')))
        || lower.starts_with("api error:")
        || lower.starts_with("prompt is too long")
        || lower.starts_with("request timed out")
        || lower.starts_with("invalid api key")
        || lower.starts_with("image was too large")
    {
        return true;
    }
    static META_SILENCE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\bsilence is\b|\bstay(s|ing)? silent\b").expect("valid regex")
    });
    static BARE_SILENCE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\W*silence\W*$").expect("valid regex"));
    static PREFIXED_LABEL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\w+:\s").expect("valid regex"));
    static MULTIPLE_SENTENCES: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[.!?]\s+[A-Z]").expect("valid regex"));
    static EVALUATIVE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"thanks|thank you|looks good|sounds good|that works|that worked|that's all|nice|great|perfect|makes sense|awesome|excellent")
            .expect("valid regex")
    });
    static CLAUDE_VOICE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(let me|i'll|i've|i'm|i can|i would|i think|i notice|here's|here is|here are|that's|this is|this will|you can|you should|you could|sure,|of course|certainly)")
            .expect("valid regex")
    });
    if PREFIXED_LABEL.is_match(suggestion)
        || words > 12
        || suggestion.encode_utf16().count() >= 100
        || MULTIPLE_SENTENCES.is_match(suggestion)
        || suggestion.contains('\n')
        || suggestion.contains('*')
        || EVALUATIVE.is_match(&lower)
        || CLAUDE_VOICE.is_match(suggestion)
    {
        return true;
    }
    if words < 2 && !suggestion.starts_with('/') {
        const ALLOWED: &[&str] = &[
            "yes", "yeah", "yep", "yea", "yup", "sure", "ok", "okay", "push", "commit", "deploy",
            "stop", "continue", "check", "exit", "quit", "no",
        ];
        if !ALLOWED.contains(&lower.as_str()) {
            return true;
        }
    }
    false
}

/// Fire-and-forget stop-hook consumer owner.
pub async fn execute_prompt_suggestion(cache_safe_params: CacheSafeParams) {
    let abort_controller = AbortController::default();
    {
        let mut current = CURRENT_ABORT_CONTROLLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = current.replace(abort_controller.clone()) {
            previous.abort();
        }
    }
    let messages = cache_safe_params.fork_context_messages.clone();
    let result = try_generate_suggestion(
        abort_controller.clone(),
        messages.as_slice(),
        cache_safe_params.clone(),
    )
    .await;
    if let Err(error) = &result {
        if !abort_controller.is_aborted() {
            crate::utils::debug::log_for_debugging(&format!(
                "Prompt suggestion generation failed: {error}"
            ));
        }
    }
    if let Ok(Some(generated)) = result {
        let suggestion = generated.suggestion.clone();
        cache_safe_params.tool_use_context.set_app_state(|state| {
            state.prompt_suggestion.text = Some(generated.suggestion);
            state.prompt_suggestion.prompt_id =
                Some(generated.prompt_id.official_name().to_string());
            state.prompt_suggestion.shown_at = 0;
            state.prompt_suggestion.accepted_at = 0;
            state.prompt_suggestion.generation_request_id = generated.generation_request_id;
        });
        if super::speculation::is_speculation_enabled() {
            let speculation_cache = cache_safe_params.clone();
            tokio::spawn(async move {
                let _ = super::speculation::start_speculation(suggestion, speculation_cache, false)
                    .await;
            });
        }
    }
    let mut current = CURRENT_ABORT_CONTROLLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if current
        .as_ref()
        .is_some_and(|current| current.same_identity(&abort_controller))
    {
        *current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str, usage: Option<crate::types::message::TokenUsage>) -> Message {
        Message::Assistant(crate::types::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: None,
            stop_reason: None,
            usage,
        })
    }

    #[test]
    fn prompt_suggestion_enablement_uses_official_precedence() {
        let cohort_enabled = crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::PromptSuggestion,
        );
        assert_eq!(
            should_enable_prompt_suggestion_with_snapshots(None, false, false, None),
            cohort_enabled
        );
        assert!(!should_enable_prompt_suggestion_with_snapshots(
            None, true, false, None
        ));
        assert!(should_enable_prompt_suggestion_with_snapshots(
            Some(false),
            true,
            true,
            Some("1")
        ));
        assert!(!should_enable_prompt_suggestion_with_snapshots(
            Some(true),
            false,
            false,
            Some("0")
        ));
    }

    #[test]
    fn prompt_suggestion_cohort_ignores_growthbook_delivery() {
        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_chomp_inflection".to_string(),
            serde_json::json!(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_chomp_inflection".to_string(),
            serde_json::json!(true),
        )]));
        crate::utils::config::set_test_global_config(Some(config));

        // The injected cache is inert; only CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION
        // still opts in.
        assert_eq!(
            should_enable_prompt_suggestion_with_snapshots(None, false, false, None),
            crate::utils::feature_flags::feature_enabled(
                crate::utils::feature_flags::FeatureFlag::PromptSuggestion,
            )
        );
        assert!(!should_enable_prompt_suggestion_with_snapshots(
            None, false, false, None
        ));
        assert!(should_enable_prompt_suggestion_with_snapshots(
            None,
            false,
            false,
            Some("1")
        ));

        crate::utils::config::set_test_global_config(None);
    }

    #[test]
    fn external_current_limits_gate_matches_official_non_allowed_semantics() {
        use crate::services::claude_ai_limits::{ClaudeAiLimits, QuotaStatus};
        use crate::utils::build_profile::BuildAudience;

        let mut state = AppState::default();
        state.prompt_suggestion_enabled = true;
        let mut limits = ClaudeAiLimits::default();
        assert_eq!(
            get_suggestion_suppress_reason_with_limits(&state, BuildAudience::External, &limits,),
            None
        );

        limits.status = QuotaStatus::AllowedWarning;
        assert_eq!(
            get_suggestion_suppress_reason_with_limits(&state, BuildAudience::External, &limits,),
            Some("rate_limit")
        );
        limits.status = QuotaStatus::Rejected;
        assert_eq!(
            get_suggestion_suppress_reason_with_limits(&state, BuildAudience::External, &limits,),
            Some("rate_limit")
        );
        assert_eq!(
            get_suggestion_suppress_reason_with_limits(
                &state,
                BuildAudience::AnthropicInternal,
                &limits,
            ),
            None
        );
    }

    #[test]
    fn suggestion_filters_match_official_boundaries() {
        assert!(should_filter_suggestion(
            Some("looks good"),
            PromptVariant::UserIntent
        ));
        assert!(should_filter_suggestion(
            Some("silence"),
            PromptVariant::UserIntent
        ));
        assert!(should_filter_suggestion(
            Some("maybe"),
            PromptVariant::UserIntent
        ));
        assert!(!should_filter_suggestion(
            Some("yes"),
            PromptVariant::UserIntent
        ));
        assert!(!should_filter_suggestion(
            Some("run the tests"),
            PromptVariant::UserIntent
        ));
        assert!(!should_filter_suggestion(
            Some("/compact"),
            PromptVariant::UserIntent
        ));
    }

    #[test]
    fn last_api_error_suppresses_generation() {
        let messages = vec![
            assistant("first", None),
            assistant("second", None),
            Message::System(crate::types::message::SystemMessage::ApiError {
                base: crate::types::message::SystemBase::new(),
                error: "API Error".to_string(),
                retry_in_ms: 0,
                retry_attempt: 0,
                max_retries: 0,
            }),
        ];
        assert!(last_response_is_api_error(&messages));
    }

    #[test]
    fn parent_cache_guard_counts_input_write_and_output() {
        let cold = assistant(
            "done",
            Some(crate::types::message::TokenUsage {
                input_tokens: 8_000,
                cache_creation_input_tokens: 2_000,
                output_tokens: 1,
                ..Default::default()
            }),
        );
        let Message::Assistant(cold) = cold else {
            unreachable!()
        };
        assert_eq!(
            get_parent_cache_suppress_reason(Some(&cold)),
            Some("cache_cold")
        );
    }

    #[derive(Clone, Debug)]
    struct SuggestionDeps {
        request: std::sync::Arc<std::sync::Mutex<Option<crate::query::deps::CallModelRequest>>>,
    }

    impl crate::query::deps::QueryDeps for SuggestionDeps {
        fn call_model(
            &self,
            request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            *self.request.lock().unwrap() = Some(request);
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(3);
                tx.send(crate::services::api::claude::QueryModelStreamItem::Stream(
                    crate::types::message::StreamEvent::ApiEvent {
                        event: serde_json::json!({
                            "type": "message_start",
                            "request_id": "req_suggestion_1"
                        }),
                        ttft_ms: Some(5),
                    },
                ))
                .await
                .ok();
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![
                                AssistantContent::Text("  run the tests  ".to_string()),
                                AssistantContent::MessageIdentity(
                                    crate::types::message::AssistantMessageIdentity {
                                        request_id: Some("req_suggestion_1".to_string()),
                                        api_message_id: Some("msg_suggestion_1".to_string()),
                                        ..Default::default()
                                    },
                                ),
                            ],
                            model: None,
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: None,
                        },
                    ),
                )
                .await
                .ok();
                Ok(rx)
            })
        }
    }

    #[tokio::test]
    async fn generation_preserves_parent_cache_parameters_and_skips_writes() {
        let request = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut context = crate::tool::ToolUseContext::default();
        context.main_loop_model = Some("claude-test".to_string());
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context,
            fork_context_messages: std::sync::Arc::new(vec![
                assistant("first", None),
                assistant("second", None),
            ]),
        };

        let suggestion = generate_suggestion_with_deps(
            AbortController::default(),
            PromptVariant::UserIntent,
            cache,
            SuggestionDeps {
                request: request.clone(),
            },
        )
        .await
        .unwrap();

        assert_eq!(suggestion.suggestion.as_deref(), Some("run the tests"));
        assert_eq!(
            suggestion.generation_request_id.as_deref(),
            Some("req_suggestion_1")
        );
        let request = request.lock().unwrap().clone().expect("request");
        assert_eq!(
            request.query_source,
            crate::constants::query_source::QuerySource::PromptSuggestion
        );
        assert_eq!(request.options.skip_cache_write, Some(true));
        assert_eq!(request.options.max_output_tokens_override, None);
    }
}
