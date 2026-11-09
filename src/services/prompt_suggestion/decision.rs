//! Decision Provider seam for prompt suggestion generation
//! (decision-provider 4.6, slice K).
//!
//! @nyan: `prompt_suggestion.generate_gate` is deliberately one-directional:
//! the deterministic decision is to generate, and the model answer can only
//! skip a generation the session was already about to make. It can never
//! trigger generation, choose the suggestion text, or change the main model.
//!
//! The gate is consulted only from `try_generate_suggestion`, which already
//! returned early when the suggestion feature is off, so the point never runs
//! while the feature is disabled. In `shadow` (the default) the request is
//! sent and recorded and generation proceeds unchanged. In `active`, only a
//! confident "not worth generating" answer skips the generation; every
//! fallback (disabled, down, timeout, low confidence, cancellation) keeps the
//! deterministic path.

use std::collections::BTreeMap;

use serde_json::json;

use crate::services::decision::{
    Answer, DecisionPoint, DecisionProvider, DeterministicAnswer, NOUL_THRESHOLD, Question, SafeSet,
};
use crate::tool::AbortController;
use crate::types::message::Message;

/// Asked before a suggestion is generated. Phrasing matters: a "yes" means the
/// conversation points at a specific next input, not that a suggestion would be
/// nice to have.
const WORTH_GENERATING_INSTRUCTIONS: &str = "Given the recent conversation, is the user's next input specific enough that showing a one-line prompt suggestion is worth generating? Answer yes only when the conversation strongly implies a concrete next text the user would type; answer no when the next step is unclear, the user needs to read or think, or the completed turn does not point to an obvious follow-up.";

const WORTH_GENERATING_QUESTION: &str = "worth_generating";

/// Maximum characters of the conversation sent to the classifier. The gate
/// only needs the shape of the last turns; a large excerpt buys no accuracy
/// and costs latency.
const MAX_EXCERPT_CHARS: usize = 4_000;
const MAX_MESSAGE_CHARS: usize = 1_000;

/// The process Decision Provider for the current settings. `None` when there is
/// no `nyan` block at all.
pub fn provider() -> Option<std::sync::Arc<DecisionProvider>> {
    let settings = crate::utils::settings::get_initial_settings();
    settings
        .nyan
        .as_ref()
        .map(|nyan| std::sync::Arc::new(DecisionProvider::from_nyan(nyan)))
}

/// Whether an active gate answer says the generation should be skipped.
///
/// `false` covers every non-skipping case — off, shadow, no provider, any
/// fallback, and a confident "generate" — so callers can treat it as "keep the
/// deterministic path".
pub async fn should_skip_generation(messages: &[Message], abort: &AbortController) -> bool {
    let Some(provider) = provider() else {
        return false;
    };
    assess(&provider, messages, abort).await
}

async fn assess(
    provider: &DecisionProvider,
    messages: &[Message],
    abort: &AbortController,
) -> bool {
    let state = json!({"conversation": conversation_excerpt(messages)});
    let questions = [(
        WORTH_GENERATING_QUESTION.to_string(),
        Question::noul(WORTH_GENERATING_INSTRUCTIONS),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    // The session was already about to generate, so "yes" agrees with the
    // deterministic answer; only a confident "no" can change behaviour.
    let deterministic = [(
        WORTH_GENERATING_QUESTION.to_string(),
        DeterministicAnswer::Noul(true),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();

    // No safe-set constraint: both answers are well formed, and the active
    // branch below applies only the skip direction. The point's confidence
    // gate still applies. Shadow returns `Fallback(Shadow)`, so `answers()` is
    // `None` and generation proceeds.
    let outcome = provider
        .decide(
            DecisionPoint::PromptSuggestionGenerateGate,
            state,
            questions,
            &deterministic,
            &SafeSet::any(),
            abort,
        )
        .await;
    // The unified decision log; `off`/disabled points do not reach a backend
    // and are not logged.
    crate::services::decision::log::log_outcome(
        DecisionPoint::PromptSuggestionGenerateGate,
        &outcome,
    );

    match outcome
        .answers()
        .and_then(|answers| answers.get(WORTH_GENERATING_QUESTION))
    {
        Some(Answer::Noul { probability }) => *probability < NOUL_THRESHOLD,
        _ => false,
    }
}

/// Last few user/assistant text turns, oldest first. Tool rows, attachments
/// and progress rows are omitted: the classifier only judges what the user and
/// assistant said.
fn conversation_excerpt(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    let mut total = 0usize;
    for message in messages.iter().rev() {
        let (role, text) = match message {
            Message::User(message) => (
                "User",
                crate::utils::messages::get_user_message_text(message),
            ),
            Message::Assistant(_) => (
                "Assistant",
                crate::utils::messages::get_assistant_message_text(message),
            ),
            _ => continue,
        };
        let Some(text) = text else {
            continue;
        };
        let text: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
        let line = format!("{role}: {text}");
        total += line.chars().count();
        if total > MAX_EXCERPT_CHARS {
            break;
        }
        parts.push(line);
    }
    parts.reverse();
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::decision::{
        BackendFuture, DecisionBackend, DecisionOutcome, DecisionRequest, DecisionUsage,
        FallbackReason, Receipt,
    };
    use crate::types::message::{AssistantContent, AssistantMessage, UserContent, UserMessage};
    use crate::utils::settings::nyan::{DecisionMode, DecisionPointConfig, DecisionSettings};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct MockBackend {
        outcome: Mutex<Option<DecisionOutcome>>,
        requests: Mutex<Vec<DecisionRequest>>,
    }

    impl MockBackend {
        fn new(outcome: DecisionOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome: Mutex::new(Some(outcome)),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl DecisionBackend for MockBackend {
        fn decide<'a>(
            &'a self,
            req: &'a DecisionRequest,
            _deadline: Duration,
            _abort: &'a AbortController,
        ) -> BackendFuture<'a> {
            self.requests.lock().unwrap().push(req.clone());
            let outcome = self
                .outcome
                .lock()
                .unwrap()
                .take()
                .expect("mock backend called twice");
            Box::pin(async move { outcome })
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    fn settings(mode: DecisionMode) -> DecisionSettings {
        let mut settings = DecisionSettings::default();
        settings.enabled = Some(true);
        settings.points = Some(
            [(
                "prompt_suggestion.generate_gate".to_string(),
                DecisionPointConfig {
                    mode: Some(mode),
                    model: None,
                    min_confidence: Some(0.75),
                    max_prob: None,
                    keep_at_least: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        settings
    }

    fn answered(probability: f64) -> DecisionOutcome {
        let mut answers = BTreeMap::new();
        answers.insert(
            WORTH_GENERATING_QUESTION.to_string(),
            Answer::Noul { probability },
        );
        DecisionOutcome::Answered {
            receipt: Receipt {
                backend: "mock".into(),
                model: "mock-model".into(),
                usage: Some(DecisionUsage {
                    input_tokens: 10,
                    output_tokens: 1,
                }),
                answers: answers.clone(),
                ..Default::default()
            },
            answers,
        }
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(text.to_string())],
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

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: None,
            stop_reason: None,
            usage: None,
        })
    }

    fn messages() -> Vec<Message> {
        vec![
            user("fix the failing test"),
            assistant("I fixed it; the suite passes now."),
        ]
    }

    async fn skip(mode: DecisionMode, outcome: DecisionOutcome) -> (bool, usize) {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-suggestion-decision-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let backend = MockBackend::new(outcome);
        let provider_backend: Arc<dyn DecisionBackend> = backend.clone();
        let provider = DecisionProvider::with_backend(settings(mode), provider_backend);
        let skip = assess(&provider, &messages(), &AbortController::default()).await;
        let calls = backend.requests.lock().unwrap().len();
        let _ = std::fs::remove_dir_all(&dir);
        (skip, calls)
    }

    #[tokio::test]
    async fn active_skips_only_a_confident_not_worth_generating_answer() {
        let (skipped, calls) = skip(DecisionMode::Active, answered(0.01)).await;
        assert!(skipped, "a confident 'no' skips the generation");
        assert_eq!(calls, 1, "the gate asks exactly once");

        let (skipped, _) = skip(DecisionMode::Active, answered(0.95)).await;
        assert!(!skipped, "a 'yes' keeps the deterministic generation");

        let (skipped, _) = skip(DecisionMode::Active, answered(0.2)).await;
        assert!(
            !skipped,
            "below minConfidence the deterministic generation wins"
        );
    }

    #[tokio::test]
    async fn shadow_requests_and_logs_but_always_generates() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-suggestion-decision-shadow-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let provider_backend: Arc<dyn DecisionBackend> = MockBackend::new(answered(0.01));
        let provider =
            DecisionProvider::with_backend(settings(DecisionMode::Shadow), provider_backend);
        let skip = assess(&provider, &messages(), &AbortController::default()).await;
        assert!(!skip, "shadow never skips generation");

        let text = std::fs::read_to_string(crate::services::decision::log::log_path()).unwrap();
        let record: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(record["point"], "prompt_suggestion.generate_gate");
        assert_eq!(record["outcome"], "shadow");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn off_does_not_reach_the_backend_and_generates() {
        let (skip, calls) = skip(DecisionMode::Off, answered(0.01)).await;
        assert!(!skip);
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn the_request_carries_the_conversation_and_the_deterministic_yes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-suggestion-decision-state-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let backend = MockBackend::new(answered(0.01));
        let provider_backend: Arc<dyn DecisionBackend> = backend.clone();
        let provider =
            DecisionProvider::with_backend(settings(DecisionMode::Active), provider_backend);
        assess(&provider, &messages(), &AbortController::default()).await;

        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].point,
            DecisionPoint::PromptSuggestionGenerateGate
        );
        let conversation = requests[0].state["conversation"].as_str().unwrap();
        assert!(conversation.contains("User: fix the failing test"));
        assert!(conversation.contains("Assistant: I fixed it"));
        assert_eq!(
            requests[0].questions[WORTH_GENERATING_QUESTION],
            Question::noul(WORTH_GENERATING_INSTRUCTIONS)
        );
        drop(requests);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_backend_fallback_keeps_generation() {
        let (skip, _) = skip(
            DecisionMode::Active,
            DecisionOutcome::Fallback {
                reason: FallbackReason::Timeout,
                receipt: Receipt::default(),
            },
        )
        .await;
        assert!(!skip);
    }

    #[test]
    fn the_excerpt_keeps_the_recent_turns_and_drops_non_text_rows() {
        let messages = [
            user("older request"),
            assistant("older answer"),
            user("newest request"),
            assistant("newest answer"),
        ];
        let excerpt = conversation_excerpt(&messages);
        assert!(excerpt.ends_with("Assistant: newest answer"));
        assert!(excerpt.contains("User: older request"));
        assert!(!excerpt.contains("System"));
    }
}
