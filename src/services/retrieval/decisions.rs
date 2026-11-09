//! @nyan: Decision Provider seams for retrieval (docs/nyan/retrieval.md 4.11,
//! decision-provider.md 4).
//!
//! Four points, each with the deterministic rule as fallback:
//! - `retrieval.intent` (active): definition / callers / concept / literal.
//!   Only changes the symbol weight and whether caller positions are returned.
//! - `retrieval.answers` (active): after Inspect and rerank, annotate the
//!   candidates that answer the query and move them first. Never deletes.
//! - `retrieval.sufficient` (active): when the top results are judged
//!   insufficient, run at most one extra round.
//! - `retrieval.prune` (shadow): request and record only. A prune answer can
//!   never remove a candidate while the point stays shadow, and the design
//!   keeps it shadow until evaluation says otherwise.
//!
//! Every point obeyes the same contract: no decision changes Inspect, the
//! "no matches" rule, or which deterministic branches run. All three
//! deterministic branches always execute; the safe set therefore never
//! contains a "skip a branch" answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::services::decision::{
    Answer, DecisionOutcome, DecisionPoint, DecisionProvider, DeterministicAnswer, Question,
    SafeAnswer, SafeSet, probe,
};
use crate::tool::AbortController;

use super::fusion;
use super::outcome::{Candidate, DecisionRecord, SearchOutcome};

/// Maximum candidates judged by `retrieval.answers` in one request. The
/// default settings cap a batch at 30 and the fused list is already ≤ 50.
const MAX_JUDGED: usize = 30;
/// Snippet characters per judged candidate. Long enough to judge, short
/// enough that 30 candidates stay inside one fast-model request.
const JUDGED_SNIPPET_CHARS: usize = 1_200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Intent {
    Definition,
    Callers,
    #[default]
    Concept,
    Literal,
}

impl Intent {
    fn as_str(self) -> &'static str {
        match self {
            Intent::Definition => "definition",
            Intent::Callers => "callers",
            Intent::Concept => "concept",
            Intent::Literal => "literal",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "definition" => Intent::Definition,
            "callers" => Intent::Callers,
            "concept" => Intent::Concept,
            "literal" => Intent::Literal,
            _ => return None,
        })
    }
}

/// 4.3 deterministic intent rules, also the Decision Provider fallback.
pub fn deterministic_intent(query: &str) -> Intent {
    let trimmed = query.trim();
    if fusion::asks_for_relations(trimmed) {
        return Intent::Callers;
    }
    if trimmed.split_whitespace().count() == 1 && fusion::symbol_weight(trimmed) > 1.0 {
        return Intent::Definition;
    }
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
        return Intent::Literal;
    }
    Intent::Concept
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IntentDecision {
    pub intent: Intent,
    pub symbol_weight: f64,
    pub relation: bool,
}

impl IntentDecision {
    pub fn deterministic(query: &str) -> Self {
        let intent = deterministic_intent(query);
        Self {
            symbol_weight: if matches!(intent, Intent::Definition | Intent::Callers) {
                2.0
            } else {
                1.0
            },
            relation: intent == Intent::Callers,
            intent,
        }
    }
}

/// Ask for the query intent. Any fallback keeps the deterministic decision
/// exactly as if the provider were absent.
pub async fn decide_intent(
    provider: Option<&DecisionProvider>,
    query: &str,
    abort: &AbortController,
    decisions: &mut Vec<DecisionRecord>,
) -> IntentDecision {
    let deterministic = IntentDecision::deterministic(query);
    let Some(provider) = provider else {
        return deterministic;
    };
    let state = json!({"query": query});
    let questions = [(
        "intent".to_string(),
        Question::choice(
            "Classify the code search query. definition = looking for where a named symbol is defined; callers = who calls or depends on a symbol; concept = natural-language description of behavior; literal = an exact string or path was quoted.",
            &[
                ("definition", "find a symbol definition"),
                ("callers", "find callers or dependencies"),
                ("concept", "describe behavior in natural language"),
                ("literal", "exact string or path"),
            ],
        ),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let expected = [(
        "intent".to_string(),
        DeterministicAnswer::Choice(deterministic.intent.as_str().to_string()),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let safe = SafeSet::any().with(
        "intent",
        SafeAnswer::Choice(
            ["definition", "callers", "concept", "literal"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
        ),
    );
    let outcome = provider
        .decide(
            DecisionPoint::RetrievalIntent,
            state,
            questions,
            &expected,
            &safe,
            abort,
        )
        .await;
    record(decisions, DecisionPoint::RetrievalIntent, &outcome);
    let Some(answers) = outcome.answers() else {
        return deterministic;
    };
    let Some(Answer::Choice { choice, .. }) = answers.get("intent") else {
        return deterministic;
    };
    let Some(intent) = Intent::from_str(choice) else {
        return deterministic;
    };
    IntentDecision {
        intent,
        symbol_weight: if matches!(intent, Intent::Definition | Intent::Callers) {
            2.0
        } else {
            1.0
        },
        relation: intent == Intent::Callers || deterministic.relation,
    }
}

/// Annotate and reorder candidates by "answers the query". A stable partition:
/// annotated candidates keep their relative order, everyone else keeps theirs.
/// No candidate is removed.
pub async fn decide_answers(
    provider: Option<&DecisionProvider>,
    query: &str,
    candidates: &mut Vec<Candidate>,
    abort: &AbortController,
    decisions: &mut Vec<DecisionRecord>,
) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    let judged = candidates.len().min(MAX_JUDGED);
    if judged == 0 {
        return false;
    }
    let mut state_candidates = Vec::with_capacity(judged);
    let mut questions = BTreeMap::new();
    let mut expected = BTreeMap::new();
    for (index, candidate) in candidates.iter().take(judged).enumerate() {
        let key = format!("c{index}");
        let snippet = candidate
            .snippet
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(JUDGED_SNIPPET_CHARS)
            .collect::<String>();
        state_candidates.push(json!({
            "id": key,
            "path": candidate.path,
            "startLine": candidate.start_line,
            "endLine": candidate.end_line,
            "symbol": candidate.symbol,
            "text": snippet,
        }));
        questions.insert(
            key.clone(),
            Question::noul("Does this code snippet directly answer the search query?"),
        );
        // The deterministic path has no answer judgement; "false" is recorded
        // for receipt agreement only. No safe-set constraint applies: both yes
        // and no are valid answers, and only the yes side annotates.
        expected.insert(key.clone(), DeterministicAnswer::Noul(false));
    }
    let state = json!({"query": query, "candidates": state_candidates});
    let outcome = provider
        .decide(
            DecisionPoint::RetrievalAnswers,
            state,
            questions,
            &expected,
            &SafeSet::any(),
            abort,
        )
        .await;
    record(decisions, DecisionPoint::RetrievalAnswers, &outcome);
    let Some(answers) = outcome.answers() else {
        return false;
    };
    let mut answering = Vec::new();
    for index in 0..judged {
        let key = format!("c{index}");
        let yes = match answers.get(&key) {
            Some(Answer::Noul { probability }) => *probability >= 0.5,
            _ => false,
        };
        if yes {
            candidates[index].answers_query = true;
            answering.push(index);
        }
    }
    if answering.is_empty() {
        return true;
    }
    let mut reordered = Vec::with_capacity(candidates.len());
    for index in &answering {
        reordered.push(candidates[*index].clone());
    }
    for (index, candidate) in candidates.iter().enumerate() {
        if !answering.contains(&index) {
            reordered.push(candidate.clone());
        }
    }
    *candidates = reordered;
    true
}

/// `retrieval.sufficient`: true when the model judged the current results
/// insufficient (and the answer cleared the provider gate).
pub async fn decide_sufficient(
    provider: Option<&DecisionProvider>,
    query: &str,
    candidates: &[Candidate],
    abort: &AbortController,
    decisions: &mut Vec<DecisionRecord>,
) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    if candidates.is_empty() {
        return false;
    }
    let top = candidates
        .iter()
        .take(5)
        .map(|candidate| {
            json!({
                "path": candidate.path,
                "startLine": candidate.start_line,
                "symbol": candidate.symbol,
                "text": candidate.snippet.as_deref().unwrap_or_default().chars().take(JUDGED_SNIPPET_CHARS).collect::<String>(),
            })
        })
        .collect::<Vec<_>>();
    let state = json!({"query": query, "results": top});
    let questions = [(
        "sufficient".to_string(),
        Question::noul("Do the top results already locate the code the query asks about?"),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let expected = [("sufficient".to_string(), DeterministicAnswer::Noul(true))]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let safe = SafeSet::any();
    let outcome = provider
        .decide(
            DecisionPoint::RetrievalSufficient,
            state,
            questions,
            &expected,
            &safe,
            abort,
        )
        .await;
    record(decisions, DecisionPoint::RetrievalSufficient, &outcome);
    let Some(answers) = outcome.answers() else {
        return false;
    };
    matches!(answers.get("sufficient"), Some(Answer::Noul { probability }) if *probability < 0.5)
}

/// `retrieval.prune` is shadow by default: the request is made and recorded,
/// but the answer never removes candidates. Even if a user configures
/// `active`, this function only records — the safe set is empty and the
/// caller ignores deletion, matching the design's "shadow 只记录".
pub async fn decide_prune_shadow(
    provider: Option<&DecisionProvider>,
    query: &str,
    candidates: &[Candidate],
    abort: &AbortController,
    decisions: &mut Vec<DecisionRecord>,
) {
    let Some(provider) = provider else {
        return;
    };
    let judged = candidates.len().min(MAX_JUDGED);
    if judged == 0 {
        return;
    }
    let mut state_candidates = Vec::with_capacity(judged);
    let mut questions = BTreeMap::new();
    for (index, candidate) in candidates.iter().take(judged).enumerate() {
        let key = format!("p{index}");
        state_candidates.push(json!({
            "id": key,
            "path": candidate.path,
            "startLine": candidate.start_line,
            "symbol": candidate.symbol,
            "text": candidate.snippet.as_deref().unwrap_or_default().chars().take(JUDGED_SNIPPET_CHARS).collect::<String>(),
        }));
        questions.insert(
            key,
            Question::noul("Is this code snippet unrelated to the search query?"),
        );
    }
    let state = json!({"query": query, "candidates": state_candidates});
    let expected = BTreeMap::new();
    let outcome = provider
        .decide(
            DecisionPoint::RetrievalPrune,
            state,
            questions,
            &expected,
            &SafeSet::any(),
            abort,
        )
        .await;
    record(decisions, DecisionPoint::RetrievalPrune, &outcome);
}

/// @nyan: register post-hoc probes (decision-provider §7) for the candidates
/// the model actually sees. Only the first `shown` candidates are rendered, so
/// only those can be used later; unmarked/extra candidates are ignored here.
pub fn register_retained_probes(
    root: &Path,
    candidates: &[Candidate],
    shown: usize,
    extra_round_keys: &BTreeSet<(String, usize, usize)>,
) {
    probe::register_files(
        DecisionPoint::RetrievalAnswers,
        retained_paths(root, candidates, shown, |candidate| candidate.answers_query),
    );
    probe::register_files(
        DecisionPoint::RetrievalSufficient,
        retained_paths(root, candidates, shown, |candidate| {
            extra_round_keys.contains(&candidate.key())
        }),
    );
}

fn retained_paths(
    root: &Path,
    candidates: &[Candidate],
    shown: usize,
    keep: impl Fn(&Candidate) -> bool,
) -> Vec<PathBuf> {
    candidates
        .iter()
        .take(shown)
        .filter(|candidate| keep(candidate))
        .map(|candidate| root.join(&candidate.path))
        .collect()
}

fn record(decisions: &mut Vec<DecisionRecord>, point: DecisionPoint, outcome: &DecisionOutcome) {
    let (result, fallback) = match outcome {
        DecisionOutcome::Answered { .. } => ("answered", None),
        DecisionOutcome::Fallback { reason, .. } => ("fallback", Some(reason.as_str())),
    };
    decisions.push(DecisionRecord {
        point: point.as_str().to_string(),
        outcome: result.to_string(),
        fallback: fallback.map(str::to_string),
    });
    // The in-memory record feeds replay; the JSONL is what `/decision` reads.
    // Retrieval used to do only the former, so its points were invisible.
    crate::services::decision::log::log_outcome(point, outcome);
}

/// Convenience for tests and evaluators replaying a recorded outcome.
pub fn record_from_outcome(outcome: &SearchOutcome) -> Value {
    serde_json::to_value(&outcome.decisions).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::decision::{
        BackendFuture, DecisionBackend, DecisionOutcome, DecisionRequest, FallbackReason, Receipt,
        StateDigest,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type Reply = Box<dyn Fn(&DecisionRequest) -> DecisionOutcome + Send + Sync>;

    struct MockBackend {
        reply: Reply,
        requests: Mutex<Vec<DecisionRequest>>,
    }

    impl MockBackend {
        fn new(reply: Reply) -> Arc<Self> {
            Arc::new(Self {
                reply,
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<DecisionRequest> {
            self.requests.lock().unwrap().clone()
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
            let outcome = (self.reply)(req);
            Box::pin(async move { outcome })
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    fn answered(answers: BTreeMap<String, Answer>) -> DecisionOutcome {
        DecisionOutcome::Answered {
            receipt: Receipt {
                backend: "mock".into(),
                model: "mock".into(),
                usage: Some(crate::services::decision::DecisionUsage {
                    input_tokens: 10,
                    output_tokens: 1,
                }),
                answers: answers.clone(),
                ..Default::default()
            },
            answers,
        }
    }

    fn provider(reply: Reply) -> (DecisionProvider, Arc<MockBackend>) {
        let backend = MockBackend::new(reply);
        let mut settings = crate::utils::settings::nyan::DecisionSettings::default();
        settings.enabled = Some(true);
        settings.max_batch = Some(30);
        settings.points = Some(
            [
                "retrieval.intent",
                "retrieval.answers",
                "retrieval.sufficient",
                "retrieval.prune",
            ]
            .into_iter()
            .map(|name| {
                (
                    name.to_string(),
                    crate::utils::settings::nyan::DecisionPointConfig {
                        mode: Some(crate::utils::settings::nyan::DecisionMode::Active),
                        model: None,
                        min_confidence: Some(0.0),
                        max_prob: None,
                        keep_at_least: None,
                    },
                )
            })
            .collect(),
        );
        let provider = DecisionProvider::with_backend(
            settings,
            Arc::clone(&backend) as Arc<dyn crate::services::decision::DecisionBackend>,
        );
        (provider, backend)
    }

    fn candidate(index: usize) -> Candidate {
        Candidate {
            path: format!("src/{index}.rs"),
            start_line: index + 1,
            end_line: index + 2,
            snippet: Some(format!("fn needle_{index}() {{}}")),
            ..Candidate::default()
        }
    }

    #[test]
    fn deterministic_intent_matches_the_documented_rules() {
        assert_eq!(deterministic_intent("AuthMiddleware"), Intent::Definition);
        assert_eq!(
            deterministic_intent("who calls parse_config"),
            Intent::Callers
        );
        assert_eq!(
            deterministic_intent("how does compaction work"),
            Intent::Concept
        );
        assert_eq!(deterministic_intent("\"exact text\""), Intent::Literal);
        assert_eq!(
            IntentDecision::deterministic("AuthMiddleware").symbol_weight,
            2.0
        );
        assert_eq!(
            IntentDecision::deterministic("how does it work").symbol_weight,
            1.0
        );
    }

    #[tokio::test]
    async fn intent_answer_overrides_only_weight_and_relations() {
        let (provider, backend) = provider(Box::new(|_| {
            let mut answers = BTreeMap::new();
            answers.insert(
                "intent".to_string(),
                Answer::Choice {
                    choice: "callers".to_string(),
                    probabilities: BTreeMap::new(),
                    confidence: 0.9,
                },
            );
            answered(answers)
        }));
        let mut decisions = Vec::new();
        let decision = decide_intent(
            Some(&provider),
            "AuthMiddleware",
            &AbortController::default(),
            &mut decisions,
        )
        .await;
        assert_eq!(decision.intent, Intent::Callers);
        assert_eq!(decision.symbol_weight, 2.0);
        assert!(decision.relation);
        assert_eq!(backend.requests().len(), 1);
        assert_eq!(decisions[0].point, "retrieval.intent");
        assert_eq!(decisions[0].outcome, "answered");
    }

    #[tokio::test]
    async fn fallback_keeps_the_deterministic_decision() {
        let (provider, _) = provider(Box::new(|request| DecisionOutcome::Fallback {
            reason: FallbackReason::Timeout,
            receipt: Receipt::empty(request.point, StateDigest::default()),
        }));
        let mut decisions = Vec::new();
        let decision = decide_intent(
            Some(&provider),
            "how does compaction work",
            &AbortController::default(),
            &mut decisions,
        )
        .await;
        assert_eq!(decision.intent, Intent::Concept);
        assert_eq!(decision.symbol_weight, 1.0);
        assert!(!decision.relation);
        assert_eq!(decisions[0].fallback.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn answers_annotate_and_move_but_never_drop() {
        let (provider, backend) = provider(Box::new(|request| {
            let mut answers = BTreeMap::new();
            // Candidate 2 answers the query.
            for key in request.questions.keys() {
                answers.insert(
                    key.clone(),
                    Answer::Noul {
                        probability: if key == "c2" { 0.95 } else { 0.02 },
                    },
                );
            }
            answered(answers)
        }));
        let mut candidates = (0..4).map(candidate).collect::<Vec<_>>();
        let mut decisions = Vec::new();
        assert!(
            decide_answers(
                Some(&provider),
                "needle",
                &mut candidates,
                &AbortController::default(),
                &mut decisions,
            )
            .await
        );
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].path, "src/2.rs");
        assert!(candidates[0].answers_query);
        assert!(
            candidates[1..]
                .iter()
                .all(|candidate| !candidate.answers_query)
        );
        let sent = &backend.requests()[0];
        assert_eq!(sent.questions.len(), 4);
    }

    #[tokio::test]
    async fn sufficient_only_triggers_when_the_model_says_no() {
        let (provider, _) = provider(Box::new(|_| {
            let mut answers = BTreeMap::new();
            answers.insert("sufficient".to_string(), Answer::Noul { probability: 0.05 });
            answered(answers)
        }));
        let candidates = (0..3).map(candidate).collect::<Vec<_>>();
        let mut decisions = Vec::new();
        assert!(
            decide_sufficient(
                Some(&provider),
                "needle",
                &candidates,
                &AbortController::default(),
                &mut decisions,
            )
            .await
        );
        // No candidates → no request, no extra round.
        assert!(
            !decide_sufficient(
                Some(&provider),
                "needle",
                &[],
                &AbortController::default(),
                &mut decisions,
            )
            .await
        );
    }

    #[test]
    fn retained_probes_cover_only_marked_candidates_that_were_shown() {
        let root = Path::new("/worktree");
        let mut candidates = (0..3).map(candidate).collect::<Vec<_>>();
        candidates[0].answers_query = true;
        candidates[2].answers_query = true;
        // The third candidate is marked but outside the rendered window.
        assert_eq!(
            retained_paths(root, &candidates, 2, |candidate| candidate.answers_query),
            vec![PathBuf::from("/worktree/src/0.rs")]
        );

        let extra = [candidates[1].key(), candidates[2].key()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            retained_paths(root, &candidates, 2, |candidate| extra
                .contains(&candidate.key())),
            vec![PathBuf::from("/worktree/src/1.rs")]
        );
    }

    #[tokio::test]
    async fn prune_never_changes_the_candidate_list() {
        let (provider, _) = provider(Box::new(|request| {
            let mut answers = BTreeMap::new();
            for key in request.questions.keys() {
                answers.insert(key.clone(), Answer::Noul { probability: 0.99 });
            }
            answered(answers)
        }));
        let before = (0..3).map(candidate).collect::<Vec<_>>();
        let mut decisions = Vec::new();
        decide_prune_shadow(
            Some(&provider),
            "needle",
            &before,
            &AbortController::default(),
            &mut decisions,
        )
        .await;
        assert_eq!(before.len(), 3);
        assert_eq!(decisions.len(), 1);
    }
}
