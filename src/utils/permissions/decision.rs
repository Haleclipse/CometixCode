//! Decision Provider seam for the auto-mode classifier (decision-provider 4.6.1).
//!
//! @nyan: `permissions.yolo_stage1` is the one permission decision point. It is
//! deliberately one-directional: the deterministic auto-mode classifier stays
//! the authority, and a model answer can only add a user confirmation for an
//! action the classifier already allowed. It can never approve anything, never
//! turn a block into an allow, and never remove a prompt.
//!
//! In `shadow` (the default) the request is still sent and recorded, but the
//! deterministic allow stands. In `active`, a confident "risky" answer turns
//! that allow into an ask; every fallback (disabled, down, timeout, low
//! confidence, cancellation) keeps the deterministic path.
//!
//! Hard rules and path / credential checks run before the classifier and are
//! untouched: this module is only consulted after `classify_yolo_action`
//! returned allow.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::services::decision::{
    Answer, DecisionOutcome, DecisionPoint, DecisionProvider, DeterministicAnswer, FallbackReason,
    NOUL_THRESHOLD, Question, Receipt, SafeSet, digest_state,
};
use crate::tool::AbortController;

/// The single noul question. Phrasing matters: the model is asked whether a
/// careful developer would double-check, not whether the action is allowed.
const RISK_INSTRUCTIONS: &str = "Would a careful developer ask the user to confirm this tool action before running it? Answer yes if the action is destructive, hard to undo, touches credentials or the network, or is otherwise risky; answer no if it is a routine, predictable action.";

const RISK_QUESTION: &str = "risk";

/// The process Decision Provider for the current settings. `None` when there is
/// no `nyan` block at all; a disabled or unreachable backend is the provider's
/// own fallback, not an absence.
pub fn provider() -> Option<std::sync::Arc<DecisionProvider>> {
    #[cfg(test)]
    if let Some(provider) = test_override::get() {
        return Some(provider);
    }
    let settings = crate::utils::settings::get_initial_settings();
    settings
        .nyan
        .as_ref()
        .map(|nyan| std::sync::Arc::new(DecisionProvider::from_nyan(nyan)))
}

/// @nyan: tests install a scripted provider here; production has no such hook.
#[cfg(test)]
pub(crate) mod test_override {
    use std::sync::{Mutex, OnceLock};

    use crate::services::decision::DecisionProvider;

    fn cell() -> &'static Mutex<Option<std::sync::Arc<DecisionProvider>>> {
        static CELL: OnceLock<Mutex<Option<std::sync::Arc<DecisionProvider>>>> = OnceLock::new();
        CELL.get_or_init(|| Mutex::new(None))
    }

    pub fn set(provider: std::sync::Arc<DecisionProvider>) {
        *cell().lock().unwrap() = Some(provider);
    }

    pub fn clear() {
        *cell().lock().unwrap() = None;
    }

    pub(super) fn get() -> Option<std::sync::Arc<DecisionProvider>> {
        cell().lock().unwrap().clone()
    }
}

/// Whether the deterministic classifier's allow should become an ask.
///
/// `false` covers every non-escalating case, including shadow and all
/// fallbacks, so callers can treat it as "keep the deterministic result".
pub async fn should_escalate(
    tool_name: &str,
    input: &Value,
    cwd: Option<&Path>,
    abort_signal: Option<anthropic_sdk::AbortSignal>,
) -> bool {
    let Some(provider) = provider() else {
        return false;
    };
    assess(&provider, tool_name, input, cwd, abort_signal).await
}

/// Ask the provider with an explicit instance (tests and callers that own one).
async fn assess(
    provider: &DecisionProvider,
    tool_name: &str,
    input: &Value,
    cwd: Option<&Path>,
    abort_signal: Option<anthropic_sdk::AbortSignal>,
) -> bool {
    let state = json!({
        "tool": tool_name,
        "input": input,
        "cwd": cwd.map(|path| path.display().to_string()),
    });
    let questions = [(RISK_QUESTION.to_string(), Question::noul(RISK_INSTRUCTIONS))]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    // The deterministic classifier already allowed this action. A "no" answer
    // therefore agrees with it; a "yes" is the only answer that can escalate.
    let deterministic = [(RISK_QUESTION.to_string(), DeterministicAnswer::Noul(false))]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

    // The provider takes a controller; mirror the caller's SDK signal so an
    // interrupt cancels the request. Creating the pair here keeps the seam
    // independent of whether the permission path has a ToolUseContext.
    let abort = AbortController::default();
    if abort_signal
        .as_ref()
        .is_some_and(anthropic_sdk::AbortSignal::is_aborted)
    {
        abort.abort();
    }
    // No safe-set constraint: both answers are representative, and only the
    // caller's `true` branch changes behaviour. The confidence gate still
    // applies through the point's `minConfidence`.
    let safe = SafeSet::any();
    let request = provider.decide(
        DecisionPoint::PermissionsYoloStage1,
        state.clone(),
        questions,
        &deterministic,
        &safe,
        &abort,
    );
    let outcome = match abort_signal {
        Some(mut signal) => {
            tokio::select! {
                outcome = request => outcome,
                _ = signal.aborted() => {
                    abort.abort();
                    DecisionOutcome::Fallback {
                        reason: FallbackReason::Cancelled,
                        receipt: Receipt::empty(
                            DecisionPoint::PermissionsYoloStage1,
                            digest_state(&state),
                        ),
                    }
                }
            }
        }
        None => request.await,
    };
    // The unified decision log; no parallel permission log.
    crate::services::decision::log::log_outcome(DecisionPoint::PermissionsYoloStage1, &outcome);

    match outcome
        .answers()
        .and_then(|answers| answers.get(RISK_QUESTION))
    {
        Some(Answer::Noul { probability }) => *probability >= NOUL_THRESHOLD,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::decision::{
        BackendFuture, DecisionBackend, DecisionPoint, DecisionRequest, DecisionUsage,
    };
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
                "permissions.yolo_stage1".to_string(),
                DecisionPointConfig {
                    mode: Some(mode),
                    model: None,
                    min_confidence: Some(0.9),
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
        answers.insert(RISK_QUESTION.to_string(), Answer::Noul { probability });
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

    async fn escalate(mode: DecisionMode, outcome: DecisionOutcome) -> (bool, usize) {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-permission-decision-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let backend = MockBackend::new(outcome);
        let provider_backend: Arc<dyn DecisionBackend> = backend.clone();
        let provider = DecisionProvider::with_backend(settings(mode), provider_backend);
        let escalate = assess(
            &provider,
            "Bash",
            &json!({"command": "rm -rf target"}),
            None,
            None,
        )
        .await;
        let calls = backend.requests.lock().unwrap().len();
        let _ = std::fs::remove_dir_all(&dir);
        (escalate, calls)
    }

    #[tokio::test]
    async fn active_escalates_only_a_confident_risky_answer() {
        let (risky, calls) = escalate(DecisionMode::Active, answered(0.99)).await;
        assert!(risky);
        assert_eq!(calls, 1, "the classifier's allow is still asked once");

        let (safe, _) = escalate(DecisionMode::Active, answered(0.01)).await;
        assert!(
            !safe,
            "a confident safe answer keeps the deterministic allow"
        );

        let (uncertain, _) = escalate(DecisionMode::Active, answered(0.7)).await;
        assert!(
            !uncertain,
            "below minConfidence the deterministic allow wins"
        );
    }

    #[tokio::test]
    async fn shadow_requests_and_logs_without_escalating() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-permission-decision-shadow-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let provider_backend: Arc<dyn DecisionBackend> = MockBackend::new(answered(0.99));
        let provider =
            DecisionProvider::with_backend(settings(DecisionMode::Shadow), provider_backend);
        let escalate = assess(&provider, "Bash", &json!({"command": "ls"}), None, None).await;
        assert!(!escalate, "shadow never changes the deterministic allow");

        let text = std::fs::read_to_string(crate::services::decision::log::log_path()).unwrap();
        let record: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(record["point"], "permissions.yolo_stage1");
        assert_eq!(record["outcome"], "shadow");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn off_does_not_reach_the_backend() {
        let (escalate, calls) = escalate(DecisionMode::Off, answered(0.99)).await;
        assert!(!escalate);
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn the_request_carries_the_action_and_the_null_deterministic_answer() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "nyan-permission-decision-state-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        let backend = MockBackend::new(answered(0.99));
        let provider_backend: Arc<dyn DecisionBackend> = backend.clone();
        let provider =
            DecisionProvider::with_backend(settings(DecisionMode::Active), provider_backend);
        assess(
            &provider,
            "Bash",
            &json!({"command": "rm -rf target"}),
            Some(Path::new("/repo")),
            None,
        )
        .await;
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].point, DecisionPoint::PermissionsYoloStage1);
        assert_eq!(requests[0].state["tool"], "Bash");
        assert_eq!(requests[0].state["input"]["command"], "rm -rf target");
        assert_eq!(requests[0].state["cwd"], "/repo");
        assert_eq!(
            requests[0].questions[RISK_QUESTION],
            Question::noul(RISK_INSTRUCTIONS)
        );
        drop(requests);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
