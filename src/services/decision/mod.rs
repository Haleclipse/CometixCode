//! Decision Provider core (decision-provider.md §3, §4, §5, §8).
//!
//! @nyan: the decision model is a *classifier*, not a text generator. It
//! answers closed questions (noul / choice / score) about content the calling
//! tool already has, in roughly 0.4 s, so the main model can skip a whole turn
//! (measured p50 12.4 s per extra turn). The core here owns everything that is
//! not tool-specific:
//!
//! - the closed decision-point registry (§4) and its built-in defaults (§8);
//! - the System One request/response shapes ([`DecisionRequest`], [`Question`],
//!   [`Answer`]);
//! - `off` / `shadow` / `active` semantics (§2, §3);
//! - the safe set + confidence gate that turns a model answer into either an
//!   applied answer or a [`FallbackReason`] (§3);
//! - per-session call/token budgets (§5.3);
//! - credential redaction ([`egress`]) and receipts that keep a state hash
//!   instead of the state (§6, §7).
//!
//! It does **not** implement any module's business logic: every caller supplies
//! its own deterministic answer and safe set, and decides what to do with an
//! [`DecisionOutcome::Answered`].
//!
//! Deviation from the design sketch, deliberate: the documented
//! `DecisionBackend::decide` returns `DecisionOutcome`, and it does here too,
//! but the caller-facing entry point is [`DecisionProvider::decide`] — that is
//! where mode, budget and validation are applied. A backend only knows how to
//! ask; it must never decide whether an answer is allowed.

pub mod backends;
pub mod egress;
pub mod log;
pub mod probe;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::tool::AbortController;
use crate::utils::settings::nyan::{DecisionMode, DecisionSettings, NyanSettings};

// ════════════════════════════════════════════════════════════
// Decision points (§4)
// ════════════════════════════════════════════════════════════

/// The closed decision-point registry. Names match
/// `BUILTIN_DECISION_POINTS` in the settings module, which is the source of
/// truth for defaults and for the settings schema's key set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DecisionPoint {
    RetrievalIntent,
    RetrievalAnswers,
    RetrievalSufficient,
    RetrievalPrune,
    WebAnswers,
    WebPageAnswers,
    WebFetchFallback,
    WebQueryKind,
    WebPrune,
    MemoryExtractNow,
    MemoryExtractSkip,
    MemoryType,
    MemoryDuplicate,
    MemoryConsolidate,
    CompactionKeep,
    PermissionsYoloStage1,
    PromptSuggestionGenerateGate,
    SpeculationStart,
}

impl DecisionPoint {
    pub const ALL: &'static [DecisionPoint] = &[
        DecisionPoint::RetrievalIntent,
        DecisionPoint::RetrievalAnswers,
        DecisionPoint::RetrievalSufficient,
        DecisionPoint::RetrievalPrune,
        DecisionPoint::WebAnswers,
        DecisionPoint::WebPageAnswers,
        DecisionPoint::WebFetchFallback,
        DecisionPoint::WebQueryKind,
        DecisionPoint::WebPrune,
        DecisionPoint::MemoryExtractNow,
        DecisionPoint::MemoryExtractSkip,
        DecisionPoint::MemoryType,
        DecisionPoint::MemoryDuplicate,
        DecisionPoint::MemoryConsolidate,
        DecisionPoint::CompactionKeep,
        DecisionPoint::PermissionsYoloStage1,
        DecisionPoint::PromptSuggestionGenerateGate,
        DecisionPoint::SpeculationStart,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DecisionPoint::RetrievalIntent => "retrieval.intent",
            DecisionPoint::RetrievalAnswers => "retrieval.answers",
            DecisionPoint::RetrievalSufficient => "retrieval.sufficient",
            DecisionPoint::RetrievalPrune => "retrieval.prune",
            DecisionPoint::WebAnswers => "web.answers",
            DecisionPoint::WebPageAnswers => "web.page_answers",
            DecisionPoint::WebFetchFallback => "web.fetch_fallback",
            DecisionPoint::WebQueryKind => "web.query_kind",
            DecisionPoint::WebPrune => "web.prune",
            DecisionPoint::MemoryExtractNow => "memory.extract_now",
            DecisionPoint::MemoryExtractSkip => "memory.extract_skip",
            DecisionPoint::MemoryType => "memory.type",
            DecisionPoint::MemoryDuplicate => "memory.duplicate",
            DecisionPoint::MemoryConsolidate => "memory.consolidate",
            DecisionPoint::CompactionKeep => "compaction.keep",
            DecisionPoint::PermissionsYoloStage1 => "permissions.yolo_stage1",
            DecisionPoint::PromptSuggestionGenerateGate => "prompt_suggestion.generate_gate",
            DecisionPoint::SpeculationStart => "speculation.start",
        }
    }

    pub fn from_str(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.as_str() == name)
    }
}

// ════════════════════════════════════════════════════════════
// Questions and answers (System One §1.4)
// ════════════════════════════════════════════════════════════

#[derive(Clone, Debug, PartialEq)]
pub enum Question {
    /// Yes/no with a probability. The wire type is `noul`.
    Noul { instructions: String },
    /// Pick one option id out of a closed set.
    Choice {
        instructions: String,
        options: BTreeMap<String, String>,
    },
    /// Pick one level index out of an ordered list.
    Score {
        instructions: String,
        levels: Vec<String>,
    },
}

impl Question {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Question::Noul {
            instructions: instructions.into(),
        }
    }

    pub fn choice(instructions: impl Into<String>, options: &[(&str, &str)]) -> Self {
        Question::Choice {
            instructions: instructions.into(),
            options: options
                .iter()
                .map(|(id, label)| (id.to_string(), label.to_string()))
                .collect(),
        }
    }

    pub fn score(instructions: impl Into<String>, levels: &[&str]) -> Self {
        Question::Score {
            instructions: instructions.into(),
            levels: levels.iter().map(|level| level.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Position on the yes/no axis, 0..=1.
    Noul {
        probability: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        level: u32,
        confidence: f64,
    },
}

impl Answer {
    /// Confidence of a noul answer is the distance from the decision boundary:
    /// a confident "no" is as useful as a confident "yes".
    pub fn confidence(&self) -> f64 {
        match self {
            Answer::Noul { probability } => (probability - 0.5).abs() * 2.0,
            Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } => *confidence,
        }
    }

    /// The answer's decided value on the noul axis (>= 0.5 means yes).
    pub fn noul_decision(&self, threshold: f64) -> Option<bool> {
        match self {
            Answer::Noul { probability } => Some(*probability >= threshold),
            _ => None,
        }
    }

    /// Whether this answer agrees with the caller's deterministic answer.
    /// `None` when the kinds differ (a wiring bug) or kinds are not comparable.
    pub fn agrees_with(&self, deterministic: &DeterministicAnswer) -> Option<bool> {
        match (self, deterministic) {
            (Answer::Noul { probability }, DeterministicAnswer::Noul(expected)) => {
                Some((*probability >= 0.5) == *expected)
            }
            (Answer::Choice { choice, .. }, DeterministicAnswer::Choice(expected)) => {
                Some(choice == expected)
            }
            (Answer::Score { level, .. }, DeterministicAnswer::Score(expected)) => {
                Some(level == expected)
            }
            _ => None,
        }
    }
}

/// The deterministic answer a caller computes before asking the decision
/// model. Used only for receipt agreement — the fallback behaviour itself is
/// the caller's.
#[derive(Clone, Debug, PartialEq)]
pub enum DeterministicAnswer {
    Noul(bool),
    Choice(String),
    Score(u32),
}

/// Noul probabilities below this are treated as "no" by
/// [`Answer::noul_decision`] when a caller does not pass its own threshold.
pub const NOUL_THRESHOLD: f64 = 0.5;

// ════════════════════════════════════════════════════════════
// Request / outcome / receipt (§3)
// ════════════════════════════════════════════════════════════

#[derive(Clone, Debug)]
pub struct DecisionRequest {
    pub point: DecisionPoint,
    /// Resolved model ID (the point's `fast` / `accurate` alias expanded).
    /// Not in the design sketch, but the backend cannot resolve an alias
    /// without reading settings, and only the provider should do that.
    pub model: String,
    /// Text or JSON: the query, candidate bodies, conversation excerpt, …
    /// Redacted by the provider before it reaches a backend.
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
}

/// Everything a receipt may record about the state: a digest and a length.
/// Never the content itself (§7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateDigest {
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecisionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// §3: only service-reported values, plus the caller-side agreement check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Receipt {
    pub point: String,
    pub backend: String,
    pub model: String,
    pub latency: Duration,
    pub usage: Option<DecisionUsage>,
    pub request_id: Option<String>,
    pub answers: BTreeMap<String, Answer>,
    /// Whether each answered question agreed with the deterministic answer
    /// the caller supplied. Absent when the caller supplied none.
    pub agreement: BTreeMap<String, bool>,
    pub state: StateDigest,
}

impl Receipt {
    pub fn empty(point: DecisionPoint, state: StateDigest) -> Self {
        Receipt {
            point: point.as_str().to_string(),
            state,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// `enabled: false` or the point's mode is `off`.
    Disabled,
    /// `shadow`: the request was made and recorded, behaviour is the
    /// deterministic path.
    Shadow,
    /// Connection failure / 401 / 403 / the backend could not be built.
    BackendDown,
    /// The service rejected the configured model id. The endpoint is up; the
    /// model alias or the account's entitlement is wrong.
    ModelNotFound,
    Timeout,
    RateLimited,
    BudgetExhausted,
    /// Credential-shaped content could not be safely stripped.
    EgressDenied,
    LowConfidence,
    OutOfSafeSet,
    InvalidResponse,
    Cancelled,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::Disabled => "disabled",
            FallbackReason::Shadow => "shadow",
            FallbackReason::BackendDown => "backend_down",
            FallbackReason::ModelNotFound => "model_not_found",
            FallbackReason::Timeout => "timeout",
            FallbackReason::RateLimited => "rate_limited",
            FallbackReason::BudgetExhausted => "budget_exhausted",
            FallbackReason::EgressDenied => "egress_denied",
            FallbackReason::LowConfidence => "low_confidence",
            FallbackReason::OutOfSafeSet => "out_of_safe_set",
            FallbackReason::InvalidResponse => "invalid_response",
            FallbackReason::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug)]
pub enum DecisionOutcome {
    Answered {
        answers: BTreeMap<String, Answer>,
        receipt: Receipt,
    },
    /// The caller must take its deterministic path.
    Fallback {
        reason: FallbackReason,
        receipt: Receipt,
    },
}

impl DecisionOutcome {
    pub fn fallback_reason(&self) -> Option<FallbackReason> {
        match self {
            DecisionOutcome::Answered { .. } => None,
            DecisionOutcome::Fallback { reason, .. } => Some(*reason),
        }
    }

    pub fn receipt(&self) -> &Receipt {
        match self {
            DecisionOutcome::Answered { receipt, .. }
            | DecisionOutcome::Fallback { receipt, .. } => receipt,
        }
    }

    pub fn answers(&self) -> Option<&BTreeMap<String, Answer>> {
        match self {
            DecisionOutcome::Answered { answers, .. } => Some(answers),
            DecisionOutcome::Fallback { .. } => None,
        }
    }
}

/// The state digest contract shared by backends and the provider.
pub fn digest_state(state: &Value) -> StateDigest {
    let (sha256, bytes) = egress::state_digest(state);
    StateDigest { sha256, bytes }
}

// ════════════════════════════════════════════════════════════
// Safe sets and the confidence gate (§3, §4)
// ════════════════════════════════════════════════════════════

/// The values a point is allowed to produce. An answer outside the set is a
/// [`FallbackReason::OutOfSafeSet`]. A rule for a question the model did not
/// answer is also out of set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SafeSet {
    rules: BTreeMap<String, SafeAnswer>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SafeAnswer {
    /// Noul must decide this boolean.
    Bool(bool),
    /// Choice must be one of these option ids.
    Choice(Vec<String>),
    /// Score must be one of these level indices.
    Score(Vec<u32>),
}

impl SafeSet {
    /// No constraint beyond a well-formed answer.
    pub fn any() -> Self {
        SafeSet::default()
    }

    pub fn with(mut self, question: impl Into<String>, answer: SafeAnswer) -> Self {
        self.rules.insert(question.into(), answer);
        self
    }

    pub fn rule(&self, question: &str) -> Option<&SafeAnswer> {
        self.rules.get(question)
    }

    fn check(&self, answers: &BTreeMap<String, Answer>) -> Result<(), FallbackReason> {
        for (question, rule) in &self.rules {
            let Some(answer) = answers.get(question) else {
                return Err(FallbackReason::OutOfSafeSet);
            };
            let allowed = match (rule, answer) {
                (SafeAnswer::Bool(expected), Answer::Noul { probability }) => {
                    (*probability >= NOUL_THRESHOLD) == *expected
                }
                (SafeAnswer::Choice(allowed), Answer::Choice { choice, .. }) => {
                    allowed.iter().any(|option| option == choice)
                }
                (SafeAnswer::Score(allowed), Answer::Score { level, .. }) => {
                    allowed.contains(level)
                }
                // A kind mismatch is a caller wiring bug; treating it as out of
                // set keeps the safe behaviour (deterministic path).
                _ => false,
            };
            if !allowed {
                return Err(FallbackReason::OutOfSafeSet);
            }
        }
        Ok(())
    }
}

/// Every answer's confidence must clear the point's threshold. For a batch
/// this is all-or-nothing: one uncertain candidate sends the whole request back
/// to the deterministic path, which is the conservative reading of §3.
fn confidence_gate(
    answers: &BTreeMap<String, Answer>,
    min_confidence: f64,
) -> Result<(), FallbackReason> {
    if answers
        .values()
        .any(|answer| answer.confidence() < min_confidence)
    {
        return Err(FallbackReason::LowConfidence);
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════
// Session budget (§5.3)
// ════════════════════════════════════════════════════════════

/// Per-session counters. Shared process-wide because nyan runs one session per
/// process and every module spends the same person's budget. `/clear` resets it.
#[derive(Debug, Default)]
pub struct SessionBudget {
    calls: AtomicU32,
    input_tokens: AtomicU64,
}

impl SessionBudget {
    pub fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }

    pub fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }

    /// Returns false when the call cap is already reached. Otherwise charges
    /// one call and returns true.
    pub fn charge_call(&self, max_calls: u32) -> bool {
        loop {
            let current = self.calls.load(Ordering::Relaxed);
            if current >= max_calls {
                return false;
            }
            if self
                .calls
                .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn charge_input_tokens(&self, tokens: u64) {
        self.input_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    pub fn tokens_exhausted(&self, max_input_tokens: u64) -> bool {
        self.input_tokens() >= max_input_tokens
    }

    pub fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.input_tokens.store(0, Ordering::Relaxed);
    }
}

static SESSION_BUDGET: OnceLock<Arc<SessionBudget>> = OnceLock::new();

fn global_session_budget() -> Arc<SessionBudget> {
    Arc::clone(SESSION_BUDGET.get_or_init(|| Arc::new(SessionBudget::default())))
}

fn session_budget() -> &'static SessionBudget {
    SESSION_BUDGET
        .get_or_init(|| Arc::new(SessionBudget::default()))
        .as_ref()
}

/// Reset the process-wide decision budget. Call on `/clear` / new session.
pub fn reset_session_budget() {
    session_budget().reset();
}

/// `(calls, input tokens)` spent by the process-wide decision budget.
pub fn session_budget_used() -> (u32, u64) {
    (session_budget().calls(), session_budget().input_tokens())
}

/// Charge one call against the process-wide budget, for tests that need a
/// non-zero counter before a reset.
#[cfg(test)]
pub fn charge_session_budget_for_tests(max_calls: u32) -> bool {
    session_budget().charge_call(max_calls)
}

// ════════════════════════════════════════════════════════════
// Backends
// ════════════════════════════════════════════════════════════

pub type BackendFuture<'a> = Pin<Box<dyn Future<Output = DecisionOutcome> + Send + 'a>>;

/// One protocol: System One. A backend asks; it does not decide policy.
pub trait DecisionBackend: Send + Sync {
    fn decide<'a>(
        &'a self,
        req: &'a DecisionRequest,
        deadline: Duration,
        abort: &'a AbortController,
    ) -> BackendFuture<'a>;

    /// Wire name for receipts (`"emrank"`, `"mock"`).
    fn name(&self) -> &'static str;
}

/// The process-wide backend. Building it reads the key file, so it is created
/// on first use, not at startup.
fn global_backend(
    settings: &DecisionSettings,
) -> Option<Arc<backends::system_one::SystemOneClient>> {
    static BACKEND: OnceLock<Option<Arc<backends::system_one::SystemOneClient>>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            backends::emrank::from_settings(&settings.emrank())
                .map_err(|error| {
                    crate::utils::debug::log_for_debugging(&format!(
                        "DecisionProvider: emrank backend unavailable: {error}"
                    ));
                    error
                })
                .ok()
        })
        .clone()
}

/// Read a credential file by path, trimming surrounding whitespace/newlines
/// (decision-provider §8). Shared by every backend that takes a key file.
pub fn read_api_key_file(path: &str) -> anyhow::Result<String> {
    let expanded = crate::utils::permissions::path_validation::expand_tilde(path);
    let path = std::path::PathBuf::from(expanded);
    let content = std::fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
    let key = content.trim();
    if key.is_empty() {
        anyhow::bail!("{} is empty", path.display());
    }
    Ok(key.to_string())
}

// ════════════════════════════════════════════════════════════
// Provider (§3, §8)
// ════════════════════════════════════════════════════════════

/// Caller-facing entry point. Cheap to construct; the backend, connection pool and default
/// budget are process-wide.
pub struct DecisionProvider {
    settings: DecisionSettings,
    backend_override: Option<Arc<dyn DecisionBackend>>,
    budget: Arc<SessionBudget>,
}

impl DecisionProvider {
    pub fn new(settings: DecisionSettings) -> Self {
        Self {
            settings,
            backend_override: None,
            budget: global_session_budget(),
        }
    }

    pub fn from_nyan(nyan: &NyanSettings) -> Self {
        Self::new(nyan.decision())
    }

    /// For tests: bypass the global emrank cell.
    pub fn with_backend(settings: DecisionSettings, backend: Arc<dyn DecisionBackend>) -> Self {
        Self::with_backend_and_budget(settings, backend, global_session_budget())
    }

    pub fn with_backend_and_budget(
        settings: DecisionSettings,
        backend: Arc<dyn DecisionBackend>,
        budget: Arc<SessionBudget>,
    ) -> Self {
        Self {
            settings,
            backend_override: Some(backend),
            budget,
        }
    }

    pub fn settings(&self) -> &DecisionSettings {
        &self.settings
    }

    fn backend(&self) -> Option<Arc<dyn DecisionBackend>> {
        match &self.backend_override {
            Some(backend) => Some(Arc::clone(backend)),
            None => {
                global_backend(&self.settings).map(|backend| backend as Arc<dyn DecisionBackend>)
            }
        }
    }

    /// Ask the backend. `deterministic` is only used for receipt agreement;
    /// the caller applies its own fallback. `safe` and the point's configured
    /// `minConfidence` gate an `active` answer.
    pub async fn decide(
        &self,
        point: DecisionPoint,
        state: Value,
        questions: BTreeMap<String, Question>,
        deterministic: &BTreeMap<String, DeterministicAnswer>,
        safe: &SafeSet,
        abort: &AbortController,
    ) -> DecisionOutcome {
        let digest = digest_state(&state);
        let point_settings = self.settings.resolved_point(point.as_str());
        let Some(point_settings) = point_settings else {
            // The enum and the settings registry must agree; a miss is a bug.
            return DecisionOutcome::Fallback {
                reason: FallbackReason::Disabled,
                receipt: Receipt::empty(point, digest),
            };
        };

        if !self.settings.enabled() || point_settings.mode == DecisionMode::Off {
            return DecisionOutcome::Fallback {
                reason: FallbackReason::Disabled,
                receipt: Receipt::empty(point, digest),
            };
        }

        if abort.is_aborted() {
            return DecisionOutcome::Fallback {
                reason: FallbackReason::Cancelled,
                receipt: Receipt::empty(point, digest),
            };
        }

        if questions.is_empty() {
            return DecisionOutcome::Fallback {
                reason: FallbackReason::InvalidResponse,
                receipt: Receipt::empty(point, digest),
            };
        }
        if questions.len() > self.settings.max_batch() as usize {
            crate::utils::debug::log_for_debugging(&format!(
                "DecisionProvider: {} batch of {} exceeds maxBatch {}; refusing",
                point.as_str(),
                questions.len(),
                self.settings.max_batch()
            ));
            return DecisionOutcome::Fallback {
                reason: FallbackReason::InvalidResponse,
                receipt: Receipt::empty(point, digest),
            };
        }

        let budget = self.budget.as_ref();
        if budget.tokens_exhausted(self.settings.max_input_tokens_per_session())
            || !budget.charge_call(self.settings.max_calls_per_session())
        {
            return DecisionOutcome::Fallback {
                reason: FallbackReason::BudgetExhausted,
                receipt: Receipt::empty(point, digest),
            };
        }

        let Some(backend) = self.backend() else {
            return DecisionOutcome::Fallback {
                reason: FallbackReason::BackendDown,
                receipt: Receipt::empty(point, digest),
            };
        };

        // Redact before the request leaves this process (§6). Only credential
        // spans change; everything else is sent as-is by design.
        let mut state = state;
        let mut questions = questions;
        egress::redact_request_parts(&mut state, &mut questions);
        // The digest describes what is actually sent.
        let digest = digest_state(&state);
        let request = DecisionRequest {
            point,
            model: self.settings.emrank().model_id(point_settings.model),
            state,
            questions,
        };

        // Retrieval / web / compaction points run beside the deterministic
        // path under the 800 ms budget; everything else is background (§5.2).
        let deadline = self
            .settings
            .deadline(DecisionSettings::is_critical_point(point.as_str()));
        let outcome = backend.decide(&request, deadline, abort).await;

        if let Some(usage) = outcome.receipt().usage {
            budget.charge_input_tokens(usage.input_tokens);
        }

        match outcome {
            DecisionOutcome::Fallback {
                reason,
                mut receipt,
            } => {
                receipt.state = digest;
                DecisionOutcome::Fallback { reason, receipt }
            }
            DecisionOutcome::Answered {
                answers,
                mut receipt,
            } => {
                receipt.state = digest;
                receipt.agreement = deterministic
                    .iter()
                    .filter_map(|(question, expected)| {
                        answers
                            .get(question)
                            .and_then(|answer| answer.agrees_with(expected))
                            .map(|agrees| (question.clone(), agrees))
                    })
                    .collect();

                if point_settings.mode == DecisionMode::Shadow {
                    // §2: request and record, behave exactly like `off`.
                    return DecisionOutcome::Fallback {
                        reason: FallbackReason::Shadow,
                        receipt,
                    };
                }

                if let Err(reason) = safe
                    .check(&answers)
                    .and_then(|()| confidence_gate(&answers, point_settings.min_confidence))
                {
                    return DecisionOutcome::Fallback { reason, receipt };
                }

                DecisionOutcome::Answered { answers, receipt }
            }
        }
    }

    /// §5.2: warm the connection after the first user input. No-op when the
    /// provider is disabled or prewarm is off.
    pub fn prewarm(&self) {
        if !self.settings.enabled() || !self.settings.emrank().prewarm() {
            return;
        }
        // Only the concrete emrank backend knows how to warm itself; custom
        // backends (tests) have nothing to warm.
        if self.backend_override.is_none()
            && let Some(backend) = global_backend(&self.settings)
        {
            backend.spawn_prewarm();
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::nyan::DecisionPointConfig;
    use serde_json::json;

    /// A backend that returns a canned outcome and remembers one request.
    struct MockBackend {
        outcome: std::sync::Mutex<Option<DecisionOutcome>>,
        requests: std::sync::Mutex<Vec<DecisionRequest>>,
    }

    impl MockBackend {
        fn new(outcome: DecisionOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome: std::sync::Mutex::new(Some(outcome)),
                requests: std::sync::Mutex::new(Vec::new()),
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

    fn answered(answer: Answer) -> DecisionOutcome {
        let mut answers = BTreeMap::new();
        answers.insert("q".to_string(), answer);
        DecisionOutcome::Answered {
            receipt: Receipt {
                backend: "mock".into(),
                model: "mock-model".into(),
                usage: Some(DecisionUsage {
                    input_tokens: 120,
                    output_tokens: 4,
                }),
                answers: answers.clone(),
                ..Default::default()
            },
            answers,
        }
    }

    fn settings_with(mode: DecisionMode, min_confidence: f64) -> DecisionSettings {
        let mut settings = DecisionSettings::default();
        settings.enabled = Some(true);
        settings.max_calls_per_session = Some(600);
        settings.max_input_tokens_per_session = Some(3_000_000);
        settings.points = Some(
            [(
                "retrieval.answers".to_string(),
                DecisionPointConfig {
                    mode: Some(mode),
                    model: None,
                    min_confidence: Some(min_confidence),
                    max_prob: None,
                    keep_at_least: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        settings
    }

    fn questions() -> BTreeMap<String, Question> {
        [("q".to_string(), Question::noul("Does it answer?"))]
            .into_iter()
            .collect()
    }

    fn build_provider(settings: DecisionSettings, backend: Arc<MockBackend>) -> DecisionProvider {
        build_provider_with_budget(settings, backend, Arc::new(SessionBudget::default()))
    }

    fn build_provider_with_budget(
        settings: DecisionSettings,
        backend: Arc<MockBackend>,
        budget: Arc<SessionBudget>,
    ) -> DecisionProvider {
        DecisionProvider::with_backend_and_budget(settings, backend, budget)
    }

    #[tokio::test]
    async fn off_and_disabled_do_not_call_the_backend() {
        let mut globally_disabled = DecisionSettings::default();
        globally_disabled.enabled = Some(false);
        for (settings, expected) in [
            (globally_disabled, FallbackReason::Disabled),
            (
                settings_with(DecisionMode::Off, 0.6),
                FallbackReason::Disabled,
            ),
        ] {
            let backend = MockBackend::new(answered(Answer::Noul { probability: 0.9 }));
            let provider = build_provider(settings, Arc::clone(&backend));
            let verdict = provider
                .decide(
                    DecisionPoint::RetrievalAnswers,
                    json!({"query": "x"}),
                    questions(),
                    &BTreeMap::new(),
                    &SafeSet::any(),
                    &AbortController::default(),
                )
                .await;
            assert_eq!(verdict.fallback_reason(), Some(expected));
            assert!(backend.requests().is_empty());
        }
    }

    #[tokio::test]
    async fn active_answers_pass_through_when_confident_and_in_the_safe_set() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.92 }));
        let provider = build_provider(
            settings_with(DecisionMode::Active, 0.6),
            Arc::clone(&backend),
        );
        let deterministic = [("q".to_string(), DeterministicAnswer::Noul(true))]
            .into_iter()
            .collect();
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({"query": "x", "candidate": "fn f() {}"}),
                questions(),
                &deterministic,
                &SafeSet::any().with("q", SafeAnswer::Bool(true)),
                &AbortController::default(),
            )
            .await;

        let DecisionOutcome::Answered { answers, receipt } = verdict else {
            panic!("expected an answer, got {:?}", verdict.fallback_reason());
        };
        assert_eq!(answers["q"], Answer::Noul { probability: 0.92 });
        assert_eq!(receipt.agreement.get("q"), Some(&true));
        assert_eq!(receipt.state.sha256.len(), 64);
        assert_eq!(backend.requests().len(), 1);
        // The model alias from the point registry resolved to fast.
        assert_eq!(backend.requests()[0].model, "typesafe/jev-1.14.0");
    }

    #[tokio::test]
    async fn low_confidence_falls_back_to_the_deterministic_path() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.55 }));
        let provider = build_provider(settings_with(DecisionMode::Active, 0.6), backend);
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions(),
                &BTreeMap::new(),
                &SafeSet::any(),
                &AbortController::default(),
            )
            .await;
        assert_eq!(
            verdict.fallback_reason(),
            Some(FallbackReason::LowConfidence)
        );
    }

    #[tokio::test]
    async fn an_answer_outside_the_safe_set_falls_back() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let provider = build_provider(settings_with(DecisionMode::Active, 0.6), backend);
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions(),
                &BTreeMap::new(),
                // The point may only add when the answer is "no".
                &SafeSet::any().with("q", SafeAnswer::Bool(false)),
                &AbortController::default(),
            )
            .await;
        assert_eq!(
            verdict.fallback_reason(),
            Some(FallbackReason::OutOfSafeSet)
        );
    }

    #[tokio::test]
    async fn shadow_requests_and_records_but_behaves_like_off() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let provider = build_provider(
            settings_with(DecisionMode::Shadow, 0.6),
            Arc::clone(&backend),
        );
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions(),
                &BTreeMap::new(),
                &SafeSet::any(),
                &AbortController::default(),
            )
            .await;
        assert_eq!(verdict.fallback_reason(), Some(FallbackReason::Shadow));
        // The receipt still carries what the model said.
        assert!(verdict.receipt().answers.contains_key("q"));
        assert_eq!(backend.requests().len(), 1);
    }

    #[tokio::test]
    async fn the_call_cap_stops_requests_with_budget_exhausted() {
        let mut settings = settings_with(DecisionMode::Active, 0.6);
        settings.max_calls_per_session = Some(1);
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let budget = Arc::new(SessionBudget::default());
        let provider = build_provider_with_budget(settings, Arc::clone(&backend), budget);
        let run = || async {
            provider
                .decide(
                    DecisionPoint::RetrievalAnswers,
                    json!({}),
                    questions(),
                    &BTreeMap::new(),
                    &SafeSet::any(),
                    &AbortController::default(),
                )
                .await
        };
        assert!(run().await.answers().is_some());
        assert_eq!(
            run().await.fallback_reason(),
            Some(FallbackReason::BudgetExhausted)
        );
        assert_eq!(backend.requests().len(), 1);
    }

    #[tokio::test]
    async fn the_token_cap_stops_later_requests() {
        let mut settings = settings_with(DecisionMode::Active, 0.6);
        settings.max_input_tokens_per_session = Some(100);
        let budget = Arc::new(SessionBudget::default());
        let provider = build_provider_with_budget(
            settings.clone(),
            MockBackend::new(answered(Answer::Noul { probability: 0.95 })),
            Arc::clone(&budget),
        );
        // First request reports 120 input tokens, over the 100-token cap.
        assert!(
            provider
                .decide(
                    DecisionPoint::RetrievalAnswers,
                    json!({}),
                    questions(),
                    &BTreeMap::new(),
                    &SafeSet::any(),
                    &AbortController::default(),
                )
                .await
                .answers()
                .is_some()
        );
        // A fresh provider with the same settings shares the session budget.
        let provider = build_provider_with_budget(
            settings,
            MockBackend::new(answered(Answer::Noul { probability: 0.95 })),
            budget,
        );
        assert_eq!(
            provider
                .decide(
                    DecisionPoint::RetrievalAnswers,
                    json!({}),
                    questions(),
                    &BTreeMap::new(),
                    &SafeSet::any(),
                    &AbortController::default(),
                )
                .await
                .fallback_reason(),
            Some(FallbackReason::BudgetExhausted)
        );
    }

    #[tokio::test]
    async fn an_oversized_batch_is_refused_without_a_request() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let mut settings = settings_with(DecisionMode::Active, 0.6);
        settings.max_batch = Some(2);
        let provider = build_provider(settings, Arc::clone(&backend));
        let questions: BTreeMap<String, Question> = (0..3)
            .map(|index| (format!("q{index}"), Question::noul("?")))
            .collect();
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions,
                &BTreeMap::new(),
                &SafeSet::any(),
                &AbortController::default(),
            )
            .await;
        assert_eq!(
            verdict.fallback_reason(),
            Some(FallbackReason::InvalidResponse)
        );
        assert!(backend.requests().is_empty());
    }

    #[tokio::test]
    async fn credentials_in_state_and_questions_never_reach_the_backend() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let provider = build_provider(
            settings_with(DecisionMode::Active, 0.6),
            Arc::clone(&backend),
        );
        let questions: BTreeMap<String, Question> = [(
            "q".to_string(),
            Question::noul("does sk-proj-abcdefghijklmnopqrstuvwxyz012345 matter?"),
        )]
        .into_iter()
        .collect();
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({"code": "let key = \"AKIAIOSFODNN7EXAMPLE\"; let t = \"ghp_abcdefghijklmnopqrstuvwxyz0123456789\";"}),
                questions,
                &BTreeMap::new(),
                &SafeSet::any(),
                &AbortController::default(),
            )
            .await;
        assert!(verdict.answers().is_some());
        let sent = backend.requests();
        let serialized = serde_json::to_string(&sent[0].state).unwrap();
        assert!(!serialized.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!serialized.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        let Question::Noul { instructions } = &sent[0].questions["q"] else {
            panic!("noul");
        };
        assert!(!instructions.contains("sk-proj-"));
        assert!(serialized.contains(egress::REDACTED));
    }

    #[tokio::test]
    async fn an_aborted_request_returns_cancelled_without_calling_the_backend() {
        let backend = MockBackend::new(answered(Answer::Noul { probability: 0.95 }));
        let provider = build_provider(
            settings_with(DecisionMode::Active, 0.6),
            Arc::clone(&backend),
        );
        let abort = AbortController::default();
        abort.abort();
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions(),
                &BTreeMap::new(),
                &SafeSet::any(),
                &abort,
            )
            .await;
        assert_eq!(verdict.fallback_reason(), Some(FallbackReason::Cancelled));
        assert!(backend.requests().is_empty());
    }

    #[tokio::test]
    async fn a_backend_failure_is_returned_unchanged_with_its_reason() {
        let backend = MockBackend::new(DecisionOutcome::Fallback {
            reason: FallbackReason::RateLimited,
            receipt: Receipt::default(),
        });
        let provider = build_provider(settings_with(DecisionMode::Active, 0.6), backend);
        let verdict = provider
            .decide(
                DecisionPoint::RetrievalAnswers,
                json!({}),
                questions(),
                &BTreeMap::new(),
                &SafeSet::any(),
                &AbortController::default(),
            )
            .await;
        assert_eq!(verdict.fallback_reason(), Some(FallbackReason::RateLimited));
        // The provider still fills the digest for logging.
        assert_eq!(verdict.receipt().state.sha256.len(), 64);
    }

    #[test]
    fn every_decision_point_matches_the_settings_registry() {
        let registry = crate::utils::settings::nyan::decision_point_names();
        let enum_names: Vec<&str> = DecisionPoint::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(enum_names, registry);
        for point in DecisionPoint::ALL {
            assert_eq!(DecisionPoint::from_str(point.as_str()), Some(*point));
        }
        assert_eq!(DecisionPoint::from_str("retrieval.nope"), None);
    }

    #[test]
    fn answer_confidence_is_distance_from_the_decision_boundary() {
        assert!((Answer::Noul { probability: 0.97 }.confidence() - 0.94).abs() < 1e-9);
        assert!((Answer::Noul { probability: 0.03 }.confidence() - 0.94).abs() < 1e-9);
        assert!((Answer::Noul { probability: 0.5 }.confidence()).abs() < 1e-9);
        assert_eq!(
            Answer::Choice {
                choice: "a".into(),
                probabilities: BTreeMap::new(),
                confidence: 0.8,
            }
            .confidence(),
            0.8
        );
    }
}
