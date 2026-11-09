//! Decision Provider seams for WebSearch / WebFetch (decision-provider 4.2).
//!
//! @nyan: every function here is additive. A decision can add a provider
//! parameter, move likely answers to the front, append a hint, or ask for one
//! more fetch provider. None can remove a provider, delete a result, rewrite a
//! page, or turn a failed fetch into a success. Any fallback (disabled, down,
//! timeout, low confidence, out of the safe set) returns `None`/`false`, which
//! is exactly the deterministic path.
//!
//! The safe sets below are what the provider enforces; the callers then apply
//! only the one direction that is allowed.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use super::providers::QueryKind;
use super::search::SearchHit;
use crate::services::decision::{
    Answer, DecisionOutcome, DecisionPoint, DecisionProvider, NOUL_THRESHOLD, Question, SafeAnswer,
    SafeSet,
};
use crate::tool::AbortController;

/// `web.page_answers` reads the first 8000 chars (decision-provider 4.2).
const PAGE_CHARS: usize = 8000;
/// `web.fetch_fallback` reads the first 3000 chars.
const CHALLENGE_CHARS: usize = 3000;
/// The outer guard sits just above the point's own deadline so a backend that
/// ignores it still cannot hold a tool call.
const GUARD_SLACK: Duration = Duration::from_millis(100);

const KIND_OPTIONS: [(&str, &str); 4] = [
    (
        "chinese",
        "The query is written in Chinese or targets Chinese-language sources",
    ),
    (
        "tech_docs",
        "The query asks for software documentation, an API, or a code-level answer",
    ),
    (
        "news",
        "The query asks for recent events or time-sensitive information",
    ),
    ("general", "None of the above"),
];

/// The process Decision Provider for the current settings. `None` only when
/// there is no `nyan` block at all; a disabled or unreachable backend is the
/// provider's own fallback, not an absence.
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

/// @nyan: tests cannot reach the real backend (no key, no network), so tool
/// tests install a scripted provider here. Production builds have no such hook.
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

/// The decision seams for one tool call.
pub struct WebDecisions<'a> {
    provider: &'a DecisionProvider,
    abort: &'a AbortController,
}

impl<'a> WebDecisions<'a> {
    pub fn new(provider: &'a DecisionProvider, abort: &'a AbortController) -> Self {
        Self { provider, abort }
    }

    async fn ask(
        &self,
        point: DecisionPoint,
        state: Value,
        questions: BTreeMap<String, Question>,
        safe: SafeSet,
    ) -> DecisionOutcome {
        let limit = self.provider.settings().deadline(true) + GUARD_SLACK;
        let deterministic = BTreeMap::new();
        let request =
            self.provider
                .decide(point, state, questions, &deterministic, &safe, self.abort);
        let outcome = match tokio::time::timeout(limit, request).await {
            Ok(outcome) => outcome,
            Err(_) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "web decision {}: outer guard expired, using the deterministic path",
                    point.as_str()
                ));
                DecisionOutcome::Fallback {
                    reason: crate::services::decision::FallbackReason::Timeout,
                    receipt: crate::services::decision::Receipt::empty(
                        point,
                        crate::services::decision::digest_state(&Value::Null),
                    ),
                }
            }
        };
        // Every Web point shares this choke point, so logging here covers
        // query_kind, answers, prune (shadow), page_answers and fetch_fallback.
        crate::services::decision::log::log_outcome(point, &outcome);
        outcome
    }

    /// `web.query_kind`: only adds provider parameters. Every provider is still
    /// queried regardless of the answer.
    pub async fn query_kind(&self, query: &str) -> Option<QueryKind> {
        let question = Question::choice("Classify the search query in state.query.", &KIND_OPTIONS);
        let outcome = self
            .ask(
                DecisionPoint::WebQueryKind,
                json!({"query": query}),
                BTreeMap::from([("kind".to_string(), question)]),
                SafeSet::any().with(
                    "kind",
                    SafeAnswer::Choice(KIND_OPTIONS.iter().map(|(id, _)| id.to_string()).collect()),
                ),
            )
            .await;
        match outcome.answers()?.get("kind")? {
            Answer::Choice { choice, .. } => match choice.as_str() {
                "chinese" => Some(QueryKind::Chinese),
                "tech_docs" => Some(QueryKind::TechDocs),
                "news" => Some(QueryKind::News),
                "general" => Some(QueryKind::General),
                _ => None,
            },
            _ => None,
        }
    }

    /// `web.answers`: one flag per candidate, index-aligned with the first
    /// `maxBatch` hits. The caller may only reorder and annotate.
    pub async fn answers(&self, query: &str, hits: &[SearchHit]) -> Option<Vec<bool>> {
        let (state, questions, count) = self.candidate_batch(query, hits, "directly answer");
        if count == 0 {
            return None;
        }
        let outcome = self
            .ask(DecisionPoint::WebAnswers, state, questions, SafeSet::any())
            .await;
        let answers = outcome.answers()?;
        (0..count)
            .map(|index| match answers.get(&format!("c{index}"))? {
                Answer::Noul { probability } => Some(*probability >= NOUL_THRESHOLD),
                _ => None,
            })
            .collect()
    }

    /// `web.prune` stays in shadow: the request is made and logged, the result
    /// is never applied and cannot shorten the list.
    pub async fn prune_shadow(&self, query: &str, hits: &[SearchHit]) {
        let (state, questions, count) = self.candidate_batch(query, hits, "address");
        if count == 0 {
            return;
        }
        let outcome = self
            .ask(DecisionPoint::WebPrune, state, questions, SafeSet::any())
            .await;
        let prune = self.provider.settings().resolved_point("web.prune");
        let max_prob = prune
            .as_ref()
            .and_then(|point| point.max_prob)
            .unwrap_or(0.05);
        let would_drop = outcome
            .receipt()
            .answers
            .values()
            .filter(
                |answer| matches!(answer, Answer::Noul { probability } if *probability < max_prob),
            )
            .count();
        crate::utils::debug::log_for_debugging(&format!(
            "web.prune (shadow, not applied): {} {would_drop} of {count} candidate(s) below P={max_prob}",
            outcome
                .fallback_reason()
                .map_or("answered,", |reason| reason.as_str()),
        ));
    }

    /// `web.page_answers`: `true` means "the page does not appear to answer the
    /// query". Only that direction is in the safe set, so the caller can add a
    /// hint and nothing else.
    pub async fn page_lacks_answer(&self, query: &str, page: &str) -> bool {
        if query.trim().is_empty() || page.trim().is_empty() {
            return false;
        }
        let question = Question::noul(
            "Does state.page contain information that answers state.query? Answer yes if it does.",
        );
        let outcome = self
            .ask(
                DecisionPoint::WebPageAnswers,
                json!({"query": query, "page": prefix(page, PAGE_CHARS)}),
                BTreeMap::from([("answers".to_string(), question)]),
                SafeSet::any().with("answers", SafeAnswer::Bool(false)),
            )
            .await;
        outcome.answers().is_some()
    }

    /// `web.fetch_fallback`: `true` means "the directly fetched page looks like a
    /// challenge page or an empty shell". It can only upgrade a rule-approved
    /// page to "needs provider fallback"; the safe set excludes the other answer.
    pub async fn looks_like_challenge(&self, page: &str) -> bool {
        if page.trim().is_empty() {
            return false;
        }
        let question = Question::noul(
            "Is state.page a bot-challenge, access-denied, cookie-wall, or JavaScript-required shell instead of real page content? Answer yes if it is.",
        );
        let outcome = self
            .ask(
                DecisionPoint::WebFetchFallback,
                json!({"page": prefix(page, CHALLENGE_CHARS)}),
                BTreeMap::from([("challenge".to_string(), question)]),
                SafeSet::any().with("challenge", SafeAnswer::Bool(true)),
            )
            .await;
        outcome.answers().is_some()
    }

    fn candidate_batch(
        &self,
        query: &str,
        hits: &[SearchHit],
        verb: &str,
    ) -> (Value, BTreeMap<String, Question>, usize) {
        let limit = self.provider.settings().max_batch() as usize;
        let count = hits.len().min(limit);
        let candidates = hits[..count]
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                json!({
                    "id": format!("c{index}"),
                    "title": hit.title,
                    "url": hit.url,
                    "snippet": hit.snippet,
                })
            })
            .collect::<Vec<_>>();
        let questions = (0..count)
            .map(|index| {
                (
                    format!("c{index}"),
                    Question::noul(format!(
                        "Based on its title and snippet, does candidate c{index} in state.candidates likely {verb} state.query?"
                    )),
                )
            })
            .collect();
        (
            json!({"query": query, "candidates": candidates}),
            questions,
            count,
        )
    }
}

fn prefix(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::web::test_support::ScriptedBackend;

    fn hit(index: usize) -> SearchHit {
        SearchHit {
            title: format!("title {index}"),
            url: format!("https://example.com/{index}"),
            snippet: format!("snippet {index}"),
            published_at: None,
            providers: vec!["exa".to_string()],
            provider_count: 1,
            rrf_rank: index + 1,
            likely_answers: false,
        }
    }

    fn choice(id: &str) -> Answer {
        Answer::Choice {
            choice: id.to_string(),
            probabilities: BTreeMap::from([(id.to_string(), 0.95)]),
            confidence: 0.95,
        }
    }

    #[tokio::test]
    async fn query_kind_maps_each_closed_option() {
        for (id, expected) in [
            ("chinese", QueryKind::Chinese),
            ("tech_docs", QueryKind::TechDocs),
            ("news", QueryKind::News),
            ("general", QueryKind::General),
        ] {
            let (provider, backend) = ScriptedBackend::provider(move |_| {
                BTreeMap::from([("kind".to_string(), choice(id))])
            });
            let abort = AbortController::default();
            let decisions = WebDecisions::new(&provider, &abort);
            assert_eq!(decisions.query_kind("q").await, Some(expected));
            let seen = backend.requests();
            assert_eq!(seen[0].point, DecisionPoint::WebQueryKind);
            assert_eq!(seen[0].state, json!({"query": "q"}));
        }
    }

    #[tokio::test]
    async fn an_unknown_choice_is_ignored() {
        let (provider, _) =
            ScriptedBackend::provider(|_| BTreeMap::from([("kind".to_string(), choice("sports"))]));
        let abort = AbortController::default();
        assert_eq!(
            WebDecisions::new(&provider, &abort).query_kind("q").await,
            None
        );
    }

    #[tokio::test]
    async fn answers_are_one_batched_request_aligned_with_the_hits() {
        let (provider, backend) = ScriptedBackend::provider(|request| {
            request
                .questions
                .keys()
                .map(|id| {
                    let yes = id == "c1" || id == "c3";
                    (
                        id.clone(),
                        Answer::Noul {
                            probability: if yes { 0.97 } else { 0.03 },
                        },
                    )
                })
                .collect()
        });
        let abort = AbortController::default();
        let hits = (0..5).map(hit).collect::<Vec<_>>();
        let flags = WebDecisions::new(&provider, &abort)
            .answers("what is x", &hits)
            .await
            .unwrap();
        assert_eq!(flags, vec![false, true, false, true, false]);
        let seen = backend.requests();
        assert_eq!(seen.len(), 1, "one request for the whole batch");
        assert_eq!(seen[0].questions.len(), 5);
        assert_eq!(
            seen[0].state["candidates"][1]["url"],
            "https://example.com/1"
        );
    }

    #[tokio::test]
    async fn the_batch_is_capped_at_max_batch() {
        let (provider, backend) = ScriptedBackend::provider(|request| {
            request
                .questions
                .keys()
                .map(|id| (id.clone(), Answer::Noul { probability: 0.9 }))
                .collect()
        });
        let abort = AbortController::default();
        let hits = (0..45).map(hit).collect::<Vec<_>>();
        let flags = WebDecisions::new(&provider, &abort)
            .answers("q", &hits)
            .await
            .unwrap();
        assert_eq!(flags.len(), 30);
        assert_eq!(backend.requests()[0].questions.len(), 30);
    }

    #[tokio::test]
    async fn page_lacks_answer_only_acts_on_a_confident_no() {
        for (probability, expected) in [(0.02, true), (0.98, false), (0.55, false)] {
            let (provider, _) = ScriptedBackend::provider(move |_| {
                BTreeMap::from([("answers".to_string(), Answer::Noul { probability })])
            });
            let abort = AbortController::default();
            let lacks = WebDecisions::new(&provider, &abort)
                .page_lacks_answer("what is x", "some page text")
                .await;
            assert_eq!(lacks, expected, "p={probability}");
        }
    }

    #[tokio::test]
    async fn page_lacks_answer_sends_only_the_first_8000_chars_and_skips_empty_input() {
        let (provider, backend) = ScriptedBackend::provider(|_| {
            BTreeMap::from([("answers".to_string(), Answer::Noul { probability: 0.01 })])
        });
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        assert!(decisions.page_lacks_answer("q", &"a".repeat(20_000)).await);
        assert_eq!(
            backend.requests()[0].state["page"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            8000
        );
        assert!(!decisions.page_lacks_answer("  ", "page").await);
        assert!(!decisions.page_lacks_answer("q", "").await);
        assert_eq!(backend.requests().len(), 1);
    }

    #[tokio::test]
    async fn challenge_detection_only_acts_on_a_confident_yes() {
        for (probability, expected) in [(0.98, true), (0.02, false), (0.6, false)] {
            let (provider, backend) = ScriptedBackend::provider(move |_| {
                BTreeMap::from([("challenge".to_string(), Answer::Noul { probability })])
            });
            let abort = AbortController::default();
            let challenge = WebDecisions::new(&provider, &abort)
                .looks_like_challenge(&"x".repeat(5000))
                .await;
            assert_eq!(challenge, expected, "p={probability}");
            assert_eq!(
                backend.requests()[0].state["page"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .count(),
                3000
            );
        }
    }

    #[tokio::test]
    async fn prune_is_shadow_and_never_returns_a_removal() {
        let (provider, backend) = ScriptedBackend::provider(|request| {
            request
                .questions
                .keys()
                .map(|id| (id.clone(), Answer::Noul { probability: 0.001 }))
                .collect()
        });
        let abort = AbortController::default();
        let hits = (0..8).map(hit).collect::<Vec<_>>();
        // Returns nothing to apply; the request was still made and recorded.
        WebDecisions::new(&provider, &abort)
            .prune_shadow("q", &hits)
            .await;
        let seen = backend.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].point, DecisionPoint::WebPrune);
    }

    #[tokio::test]
    async fn disabled_or_off_points_send_nothing_and_return_the_deterministic_answer() {
        let (provider, backend) = ScriptedBackend::provider_with(
            |settings| settings.enabled = Some(false),
            |_| BTreeMap::new(),
        );
        let abort = AbortController::default();
        let decisions = WebDecisions::new(&provider, &abort);
        assert_eq!(decisions.query_kind("q").await, None);
        assert_eq!(decisions.answers("q", &[hit(0)]).await, None);
        assert!(!decisions.page_lacks_answer("q", "page").await);
        assert!(!decisions.looks_like_challenge("page").await);
        assert!(backend.requests().is_empty());
    }

    #[tokio::test]
    async fn an_aborted_call_never_reaches_the_backend() {
        let (provider, backend) = ScriptedBackend::provider(|_| BTreeMap::new());
        let abort = AbortController::default();
        abort.abort();
        assert_eq!(
            WebDecisions::new(&provider, &abort).query_kind("q").await,
            None
        );
        assert!(backend.requests().is_empty());
    }
}
