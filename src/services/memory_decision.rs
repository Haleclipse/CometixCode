//! @nyan: Decision Provider shadow seam for memory (decision-provider 4.3,
//! memory M10, SMOKE-EVAL-2026-11-01 C4).
//!
//! # Objective
//! The five memory decision points already exist in the closed registry
//! (`memory.extract_now`, `memory.extract_skip`, `memory.type`,
//! `memory.duplicate`, `memory.consolidate`) but the extraction and
//! consolidation flows only ever apply deterministic rules. This module asks
//! the decision model for those five points in **shadow** mode: the model's
//! answer and the deterministic answer are compared and appended to
//! `$CLAUDE_CONFIG_DIR/decision/memory.<point>.jsonl`, while the memory
//! pipeline keeps using the deterministic rule byte for byte.
//!
//! # Contract
//! 1. Each point has its own switch under `settings.nyan.memory.decisionShadow`.
//!    All default to off; off means no request, no file, no allocation.
//! 2. Shadow answers are only ever recorded. Nothing here reads, deletes,
//!    rewrites or reorders a memory file.
//! 3. A timeout, missing key, disabled backend or low confidence ends in a
//!    recorded fallback; the memory flow never waits on it. Callers spawn
//!    these evaluations as detached work.
//! 4. No provider-specific code: everything goes through [`DecisionProvider`]
//!    and its `emrank` / injected backend.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub use crate::services::decision::DecisionPoint;
use crate::services::decision::{
    Answer, DecisionOutcome, DecisionProvider, DeterministicAnswer, Question, SafeSet,
};
use crate::tool::AbortController;
use crate::types::message::Message;
use crate::utils::settings::nyan::{DecisionMode, MemorySettings};

/// The five memory points this seam covers, in log-file order.
pub const MEMORY_POINTS: &[DecisionPoint] = &[
    DecisionPoint::MemoryExtractNow,
    DecisionPoint::MemoryExtractSkip,
    DecisionPoint::MemoryType,
    DecisionPoint::MemoryDuplicate,
    DecisionPoint::MemoryConsolidate,
];

/// File-stem suffix for `$CLAUDE_CONFIG_DIR/decision/memory.<suffix>.jsonl`.
pub fn log_suffix(point: DecisionPoint) -> Option<&'static str> {
    match point {
        DecisionPoint::MemoryExtractNow => Some("extract_now"),
        DecisionPoint::MemoryExtractSkip => Some("extract_skip"),
        DecisionPoint::MemoryType => Some("type"),
        DecisionPoint::MemoryDuplicate => Some("duplicate"),
        DecisionPoint::MemoryConsolidate => Some("consolidate"),
        _ => None,
    }
}

/// The per-point switch, independent of the global Decision Provider switch.
pub fn point_switch(point: DecisionPoint, memory: &MemorySettings) -> bool {
    let shadow = memory.decision_shadow();
    match point {
        DecisionPoint::MemoryExtractNow => shadow.extract_now(),
        DecisionPoint::MemoryExtractSkip => shadow.extract_skip(),
        DecisionPoint::MemoryType => shadow.memory_type(),
        DecisionPoint::MemoryDuplicate => shadow.duplicate(),
        DecisionPoint::MemoryConsolidate => shadow.consolidate(),
        _ => false,
    }
}

/// True when this point should be queried in shadow mode: its own switch is on,
/// the Decision Provider is enabled and the point is not explicitly `off`.
pub fn shadow_enabled(point: DecisionPoint, memory: &MemorySettings) -> bool {
    if log_suffix(point).is_none() || !point_switch(point, memory) {
        return false;
    }
    let settings = provider_settings();
    settings.enabled()
        && settings
            .resolved_point(point.as_str())
            .is_some_and(|resolved| resolved.mode != DecisionMode::Off)
}

/// Convenience for the current process settings.
pub fn shadow_enabled_now(point: DecisionPoint) -> bool {
    shadow_enabled(point, &current_memory_settings())
}

pub fn current_memory_settings() -> MemorySettings {
    crate::utils::settings::get_initial_settings()
        .nyan
        .as_ref()
        .map(|nyan| nyan.memory())
        .unwrap_or_default()
}

fn provider_settings() -> crate::utils::settings::nyan::DecisionSettings {
    crate::utils::settings::get_initial_settings()
        .nyan
        .as_ref()
        .map(|nyan| nyan.decision())
        .unwrap_or_default()
}

fn provider() -> Arc<DecisionProvider> {
    #[cfg(test)]
    if let Some(provider) = test_override::get() {
        return provider;
    }
    cached_provider(provider_settings())
}

type ProviderCache = std::sync::Mutex<
    Option<(
        crate::utils::settings::nyan::DecisionSettings,
        Arc<DecisionProvider>,
    )>,
>;

/// @nyan: one provider per distinct `DecisionSettings`, so repeated shadow
/// calls share its backend (connection pool, prewarm) instead of rebuilding it.
/// Keyed by the settings value: when settings change, the next call sees a
/// different value and rebuilds. Only reached once a shadow switch is on, so
/// the default-off path never touches it.
fn cached_provider(
    settings: crate::utils::settings::nyan::DecisionSettings,
) -> Arc<DecisionProvider> {
    static CACHE: std::sync::OnceLock<ProviderCache> = std::sync::OnceLock::new();
    let mut guard = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached, provider)) = guard.as_ref()
        && *cached == settings
    {
        return Arc::clone(provider);
    }
    let provider = Arc::new(DecisionProvider::new(settings.clone()));
    *guard = Some((settings, Arc::clone(&provider)));
    provider
}

/// Log path for one memory point.
pub fn log_path(point: DecisionPoint) -> Option<PathBuf> {
    log_suffix(point).map(|suffix| {
        crate::utils::env_utils::get_claude_config_home_dir()
            .join("decision")
            .join(format!("memory.{suffix}.jsonl"))
    })
}

fn append_record<T: Serialize>(point: DecisionPoint, record: &T) -> std::io::Result<()> {
    let Some(path) = log_path(point) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(record)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    use std::io::Write;
    writeln!(file, "{line}")
}

// ════════════════════════════════════════════════════════════
// Record envelope
// ════════════════════════════════════════════════════════════

/// Fields every memory shadow record shares. Point-specific fields live in
/// `detail`, so a reader can group on `point` and still parse the payload.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryShadowRecord {
    pub timestamp: String,
    pub point: String,
    pub model: String,
    pub latency_ms: u64,
    /// `shadow` when the model answered and this seam discarded the answer.
    /// `fallback` when it did not answer. Never `applied`.
    pub outcome: String,
    /// Always false. This seam never changes extraction or consolidation.
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// §7: the state is never logged, only its digest and length.
    pub state_sha256: String,
    pub state_bytes: usize,
    pub detail: serde_json::Value,
}

struct EvaluateResult {
    answers: std::collections::BTreeMap<String, Answer>,
    outcome: &'static str,
    fallback_reason: Option<String>,
    model: String,
    latency_ms: u64,
    state_sha256: String,
    state_bytes: usize,
}

/// One shadow request. Returns `None` only when the point is disabled.
///
/// The provider already applies the point's deadline (background = 2 s by
/// default). The outer timeout is a belt-and-braces bound so a wedged backend
/// can never hold the detached task open.
async fn evaluate(
    point: DecisionPoint,
    state: serde_json::Value,
    questions: std::collections::BTreeMap<String, Question>,
    deterministic: &std::collections::BTreeMap<String, DeterministicAnswer>,
) -> Option<EvaluateResult> {
    let provider = provider();
    let started = Instant::now();
    let abort = AbortController::default();
    let deadline = provider.settings().deadline(false) + Duration::from_millis(500);
    let outcome = match tokio::time::timeout(
        deadline,
        provider.decide(
            point,
            state,
            questions,
            deterministic,
            &SafeSet::any(),
            &abort,
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => {
            return Some(EvaluateResult {
                answers: Default::default(),
                outcome: "fallback",
                fallback_reason: Some("timeout".to_string()),
                model: String::new(),
                latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                state_sha256: String::new(),
                state_bytes: 0,
            });
        }
    };
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let has_answers = !outcome.receipt().answers.is_empty();
    let receipt = outcome.receipt();
    // This seam is shadow-only: a confident answer is recorded and dropped,
    // even when the point's configured mode is `active`.
    let (outcome_str, fallback_reason) = match &outcome {
        DecisionOutcome::Answered { .. } => ("shadow", None),
        DecisionOutcome::Fallback { reason, .. } if has_answers => {
            ("shadow", Some(reason.as_str().to_string()))
        }
        DecisionOutcome::Fallback { reason, .. } => ("fallback", Some(reason.as_str().to_string())),
    };
    Some(EvaluateResult {
        answers: receipt.answers.clone(),
        outcome: outcome_str,
        fallback_reason,
        model: receipt.model.clone(),
        latency_ms,
        state_sha256: receipt.state.sha256.clone(),
        state_bytes: receipt.state.bytes,
    })
}

fn noul(answers: &std::collections::BTreeMap<String, Answer>, key: &str) -> Option<(f64, f64)> {
    match answers.get(key) {
        Some(Answer::Noul { probability }) => Some((
            *probability,
            Answer::Noul {
                probability: *probability,
            }
            .confidence(),
        )),
        _ => None,
    }
}

// ════════════════════════════════════════════════════════════
// memory.extract_now / memory.extract_skip
// ════════════════════════════════════════════════════════════

/// A bounded, prompt-safe view of the turn that just ended.
///
/// Tool results sit between the user prompt and the final reply, so the latest
/// text of each role is taken from anywhere in the slice, not from a fixed tail.
fn turn_excerpt(messages: &[Message], max_chars: usize) -> String {
    let mut user_text = None;
    let mut assistant_text = None;
    for message in messages.iter().rev() {
        match message {
            Message::User(user) if user_text.is_none() => {
                user_text = crate::utils::messages::get_user_message_text(user);
            }
            Message::Assistant(_) if assistant_text.is_none() => {
                assistant_text = crate::utils::messages::get_assistant_message_text(message);
            }
            _ => {}
        }
        if user_text.is_some() && assistant_text.is_some() {
            break;
        }
    }
    let mut parts = Vec::new();
    if let Some(text) = user_text {
        parts.push(format!("user: {text}"));
    }
    if let Some(text) = assistant_text {
        parts.push(format!("assistant: {text}"));
    }
    let joined = parts.join("\n\n");
    joined.chars().take(max_chars).collect()
}

const EXTRACT_NOW_QUESTION: &str = "Does this turn contain durable information worth saving to long-term memory? Count only facts that are not derivable from the repository or git history.";
const EXTRACT_SKIP_QUESTION: &str = "Does this conversation segment contain nothing worth saving to long-term memory, so extraction should be skipped?";

/// `memory.extract_now`: compare the model's "worth remembering" judgment with
/// the deterministic throttle outcome. `deterministic_due` is what the rule
/// decided on this turn.
pub async fn evaluate_extract_now(
    messages: &[Message],
    deterministic_due: bool,
    turns_since_extract: u32,
    token_delta: u64,
) -> Option<MemoryShadowRecord> {
    if !shadow_enabled_now(DecisionPoint::MemoryExtractNow) {
        return None;
    }
    let state = json!({
        "recentTurns": turn_excerpt(messages, 6_000),
        "turnsSinceExtract": turns_since_extract,
        "tokenDelta": token_delta,
    });
    let questions = [(
        "worth_remembering".to_string(),
        Question::noul(EXTRACT_NOW_QUESTION),
    )]
    .into_iter()
    .collect();
    let deterministic = [(
        "worth_remembering".to_string(),
        DeterministicAnswer::Noul(deterministic_due),
    )]
    .into_iter()
    .collect();
    let result = evaluate(
        DecisionPoint::MemoryExtractNow,
        state,
        questions,
        &deterministic,
    )
    .await?;
    let (probability, confidence) = noul(&result.answers, "worth_remembering")
        .map_or((None, None), |(p, c)| (Some(p), Some(c)));
    let model_says = probability.map(|p| p >= 0.5);
    let record = MemoryShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::MemoryExtractNow.as_str().to_string(),
        model: result.model,
        latency_ms: result.latency_ms,
        outcome: result.outcome.to_string(),
        applied: false,
        fallback_reason: result.fallback_reason,
        state_sha256: result.state_sha256,
        state_bytes: result.state_bytes,
        detail: json!({
            "turnsSinceExtract": turns_since_extract,
            "tokenDelta": token_delta,
            "deterministicDue": deterministic_due,
            "modelProbability": probability,
            "modelConfidence": confidence,
            "modelSaysExtractNow": model_says,
            "agrees": model_says.map(|says| says == deterministic_due),
        }),
    };
    log_outcome(&record);
    Some(record)
}

/// `memory.extract_skip`: only meaningful when the deterministic rule says
/// "extract now" (`due == true`), since the rule never skips.
pub async fn evaluate_extract_skip(
    messages: &[Message],
    turns_since_extract: u32,
    token_delta: u64,
) -> Option<MemoryShadowRecord> {
    if !shadow_enabled_now(DecisionPoint::MemoryExtractSkip) {
        return None;
    }
    let state = json!({
        "recentTurns": turn_excerpt(messages, 6_000),
        "turnsSinceExtract": turns_since_extract,
        "tokenDelta": token_delta,
    });
    let questions = [(
        "nothing_to_save".to_string(),
        Question::noul(EXTRACT_SKIP_QUESTION),
    )]
    .into_iter()
    .collect();
    let deterministic = [(
        "nothing_to_save".to_string(),
        DeterministicAnswer::Noul(false),
    )]
    .into_iter()
    .collect();
    let result = evaluate(
        DecisionPoint::MemoryExtractSkip,
        state,
        questions,
        &deterministic,
    )
    .await?;
    let (probability, confidence) =
        noul(&result.answers, "nothing_to_save").map_or((None, None), |(p, c)| (Some(p), Some(c)));
    let model_says_skip = probability.map(|p| p >= 0.5);
    let record = MemoryShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::MemoryExtractSkip.as_str().to_string(),
        model: result.model,
        latency_ms: result.latency_ms,
        outcome: result.outcome.to_string(),
        applied: false,
        fallback_reason: result.fallback_reason,
        state_sha256: result.state_sha256,
        state_bytes: result.state_bytes,
        detail: json!({
            "turnsSinceExtract": turns_since_extract,
            "tokenDelta": token_delta,
            "deterministicSkip": false,
            "modelProbability": probability,
            "modelConfidence": confidence,
            "modelSaysSkip": model_says_skip,
            "agrees": model_says_skip.map(|says| !says),
        }),
    };
    log_outcome(&record);
    Some(record)
}

// ════════════════════════════════════════════════════════════
// Memory file snapshots (type / duplicate)
// ════════════════════════════════════════════════════════════

/// One topic file as the extraction agent left it.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryFileSnapshot {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
    pub memory_type: Option<String>,
    pub body_excerpt: String,
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("logs") {
                collect_markdown_files(&path, files);
            }
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
            && path.file_name().and_then(|name| name.to_str()) != Some("MEMORY.md")
        {
            files.push(path);
        }
    }
}

fn snapshot_from_path(path: PathBuf) -> Option<MemoryFileSnapshot> {
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed = crate::utils::frontmatter_parser::parse_frontmatter(&raw);
    let name = parsed
        .frontmatter
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .map(|name| name.to_string_lossy().to_string())
        })?;
    Some(MemoryFileSnapshot {
        path,
        name,
        description: parsed
            .frontmatter
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        memory_type: parsed
            .frontmatter
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase()),
        body_excerpt: parsed.content.chars().take(1_200).collect(),
    })
}

/// Snapshot every topic memory file. Cheap enough to run only when a switch is
/// on; callers guard on [`types_or_duplicates_enabled`] first.
pub fn snapshot_memory_files() -> Vec<MemoryFileSnapshot> {
    let mut files = Vec::new();
    collect_markdown_files(&crate::memdir::paths::get_auto_mem_path(), &mut files);
    files.sort();
    files
        .iter()
        .cloned()
        .filter_map(snapshot_from_path)
        .collect()
}

/// Guard so the driver never walks the directory when both switches are off.
pub fn types_or_duplicates_enabled() -> bool {
    shadow_enabled_now(DecisionPoint::MemoryType)
        || shadow_enabled_now(DecisionPoint::MemoryDuplicate)
}

fn same_entry(a: &MemoryFileSnapshot, b: &MemoryFileSnapshot) -> bool {
    a.name == b.name
        && a.description == b.description
        && a.memory_type == b.memory_type
        && a.body_excerpt == b.body_excerpt
}

/// Files created or rewritten by the run that just finished, plus the
/// pre-run snapshot they are compared against.
pub fn changed_since(
    before: &[MemoryFileSnapshot],
    after: &[MemoryFileSnapshot],
) -> Vec<MemoryFileSnapshot> {
    after
        .iter()
        .filter(|candidate| {
            !before
                .iter()
                .any(|old| old.path == candidate.path && same_entry(old, candidate))
        })
        .cloned()
        .collect()
}

const MEMORY_TYPES: [(&str, &str); 4] = [
    (
        "user",
        "information about the user's role, goals, preferences or knowledge",
    ),
    (
        "feedback",
        "guidance about how to approach work, including reasons and how to apply it",
    ),
    (
        "project",
        "ongoing work, goals, initiatives, bugs or incidents not derivable from code or git",
    ),
    (
        "reference",
        "pointers to where information lives in external systems",
    ),
];

/// `memory.type`: ask the model to classify each new memory and compare with the
/// type the extraction agent wrote into the frontmatter.
pub async fn evaluate_types(
    changed: &[MemoryFileSnapshot],
    max_batch: usize,
) -> Option<MemoryShadowRecord> {
    if changed.is_empty() || !shadow_enabled_now(DecisionPoint::MemoryType) {
        return None;
    }
    let batch: Vec<&MemoryFileSnapshot> = changed.iter().take(max_batch.max(1)).collect();
    let state = json!({
        "newMemories": batch.iter().map(|file| json!({
            "path": file.path.display().to_string(),
            "name": file.name,
            "description": file.description,
            "body": file.body_excerpt,
        })).collect::<Vec<_>>(),
    });
    let mut questions = std::collections::BTreeMap::new();
    let mut deterministic = std::collections::BTreeMap::new();
    for file in &batch {
        let key = file.path.display().to_string();
        questions.insert(
            key.clone(),
            Question::choice("Which memory type best fits this entry?", &MEMORY_TYPES),
        );
        deterministic.insert(
            key,
            DeterministicAnswer::Choice(file.memory_type.clone().unwrap_or_default()),
        );
    }
    let result = evaluate(DecisionPoint::MemoryType, state, questions, &deterministic).await?;
    let entries: Vec<serde_json::Value> = batch
        .iter()
        .map(|file| {
            let key = file.path.display().to_string();
            let (model_type, confidence) = match result.answers.get(&key) {
                Some(Answer::Choice {
                    choice, confidence, ..
                }) => (Some(choice.clone()), Some(*confidence)),
                _ => (None, None),
            };
            json!({
                "path": key,
                "name": file.name,
                "deterministicType": file.memory_type,
                "modelType": model_type,
                "modelConfidence": confidence,
                "agrees": model_type.map(|model| file.memory_type.as_deref() == Some(model.as_str())),
            })
        })
        .collect();
    let record = MemoryShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::MemoryType.as_str().to_string(),
        model: result.model,
        latency_ms: result.latency_ms,
        outcome: result.outcome.to_string(),
        applied: false,
        fallback_reason: result.fallback_reason,
        state_sha256: result.state_sha256,
        state_bytes: result.state_bytes,
        detail: json!({ "entries": entries, "candidatesCount": batch.len() }),
    };
    log_outcome(&record);
    Some(record)
}

/// `memory.duplicate`: ask whether each new memory duplicates an existing entry.
/// The deterministic answer is "the extraction agent rewrote a pre-existing
/// topic file rather than adding a new one" (same path and same identity before
/// the run), which is exactly the update path the prompt tells it to prefer.
pub async fn evaluate_duplicates(
    before: &[MemoryFileSnapshot],
    changed: &[MemoryFileSnapshot],
    max_batch: usize,
    max_existing: usize,
) -> Option<MemoryShadowRecord> {
    if changed.is_empty() || !shadow_enabled_now(DecisionPoint::MemoryDuplicate) {
        return None;
    }
    let batch: Vec<&MemoryFileSnapshot> = changed.iter().take(max_batch.max(1)).collect();
    let existing: Vec<&MemoryFileSnapshot> = before.iter().take(max_existing.max(1)).collect();
    let state = json!({
        "existingMemories": existing.iter().map(|file| json!({
            "name": file.name,
            "type": file.memory_type,
            "description": file.description,
            "body": file.body_excerpt.chars().take(400).collect::<String>(),
        })).collect::<Vec<_>>(),
        "newMemories": batch.iter().map(|file| json!({
            "path": file.path.display().to_string(),
            "name": file.name,
            "type": file.memory_type,
            "description": file.description,
            "body": file.body_excerpt,
        })).collect::<Vec<_>>(),
    });
    let mut questions = std::collections::BTreeMap::new();
    let mut deterministic = std::collections::BTreeMap::new();
    for file in &batch {
        let key = file.path.display().to_string();
        questions.insert(
            key.clone(),
            Question::noul(
                "Is this new memory a duplicate of, or an update to, any existing memory above?",
            ),
        );
        let updated_existing = before
            .iter()
            .any(|old| old.path == file.path && old.name == file.name);
        deterministic.insert(key, DeterministicAnswer::Noul(updated_existing));
    }
    let result = evaluate(
        DecisionPoint::MemoryDuplicate,
        state,
        questions,
        &deterministic,
    )
    .await?;
    let entries: Vec<serde_json::Value> = batch
        .iter()
        .map(|file| {
            let key = file.path.display().to_string();
            let (probability, confidence) =
                noul(&result.answers, &key).map_or((None, None), |(p, c)| (Some(p), Some(c)));
            let model_says_duplicate = probability.map(|p| p >= 0.5);
            let deterministic_duplicate = before
                .iter()
                .any(|old| old.path == file.path && old.name == file.name);
            json!({
                "path": key,
                "name": file.name,
                "deterministicDuplicate": deterministic_duplicate,
                "modelProbability": probability,
                "modelConfidence": confidence,
                "modelSaysDuplicate": model_says_duplicate,
                "agrees": model_says_duplicate.map(|says| says == deterministic_duplicate),
            })
        })
        .collect();
    let record = MemoryShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::MemoryDuplicate.as_str().to_string(),
        model: result.model,
        latency_ms: result.latency_ms,
        outcome: result.outcome.to_string(),
        applied: false,
        fallback_reason: result.fallback_reason,
        state_sha256: result.state_sha256,
        state_bytes: result.state_bytes,
        detail: json!({ "entries": entries, "candidatesCount": batch.len() }),
    };
    log_outcome(&record);
    Some(record)
}

// ════════════════════════════════════════════════════════════
// memory.consolidate
// ════════════════════════════════════════════════════════════

/// Inputs the deterministic consolidation trigger already computed.
#[derive(Clone, Copy, Debug)]
pub struct ConsolidationContext {
    pub last_start_ms: Option<u64>,
    pub newest_file_ms: Option<u64>,
    pub new_file_count: usize,
    pub min_hours: u32,
    pub min_new_files: u32,
    pub now_ms: u64,
    pub deterministic_due: bool,
}

/// `memory.consolidate`: compare the model's "consolidate now" judgment with
/// the 24 h / 5-new-files rule.
pub async fn evaluate_consolidate(context: ConsolidationContext) -> Option<MemoryShadowRecord> {
    if !shadow_enabled_now(DecisionPoint::MemoryConsolidate) {
        return None;
    }
    let hours_since_last = context
        .last_start_ms
        .map(|last| context.now_ms.saturating_sub(last) as f64 / 3_600_000.0);
    let state = json!({
        "hoursSinceLastConsolidation": hours_since_last,
        "newMemoryFiles": context.new_file_count,
        "newestFileAgeHours": context.newest_file_ms.map(|mtime| {
            context.now_ms.saturating_sub(mtime) as f64 / 3_600_000.0
        }),
        "deterministicThresholds": {
            "minHours": context.min_hours,
            "minNewFiles": context.min_new_files,
        },
    });
    let questions = [(
        "consolidate_now".to_string(),
        Question::noul("Should memory consolidation run now, given the age and number of new memory files above?"),
    )]
    .into_iter()
    .collect();
    let deterministic = [(
        "consolidate_now".to_string(),
        DeterministicAnswer::Noul(context.deterministic_due),
    )]
    .into_iter()
    .collect();
    let result = evaluate(
        DecisionPoint::MemoryConsolidate,
        state,
        questions,
        &deterministic,
    )
    .await?;
    let (probability, confidence) =
        noul(&result.answers, "consolidate_now").map_or((None, None), |(p, c)| (Some(p), Some(c)));
    let model_says = probability.map(|p| p >= 0.5);
    let record = MemoryShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::MemoryConsolidate.as_str().to_string(),
        model: result.model,
        latency_ms: result.latency_ms,
        outcome: result.outcome.to_string(),
        applied: false,
        fallback_reason: result.fallback_reason,
        state_sha256: result.state_sha256,
        state_bytes: result.state_bytes,
        detail: json!({
            "lastStartMs": context.last_start_ms,
            "nowMs": context.now_ms,
            "hoursSinceLastConsolidation": hours_since_last,
            "newFileCount": context.new_file_count,
            "minHours": context.min_hours,
            "minNewFiles": context.min_new_files,
            "deterministicDue": context.deterministic_due,
            "modelProbability": probability,
            "modelConfidence": confidence,
            "modelSaysConsolidate": model_says,
            "agrees": model_says.map(|says| says == context.deterministic_due),
        }),
    };
    log_outcome(&record);
    Some(record)
}

fn log_outcome(record: &MemoryShadowRecord) {
    let Some(point) = DecisionPoint::from_str(&record.point) else {
        return;
    };
    if let Err(error) = append_record(point, record) {
        crate::utils::debug::log_for_debugging(&format!(
            "memory_decision: failed to write {} shadow log: {error}",
            record.point
        ));
    }
}

/// Run a shadow evaluation as detached work so the memory pipeline never waits
/// for it. Falls back to the current runtime handle; when neither exists (a
/// sync context) the evaluation is dropped, which is the "unavailable" path.
pub fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        return;
    };
    handle.spawn(future);
}

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

    pub fn get() -> Option<std::sync::Arc<DecisionProvider>> {
        cell().lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::decision::{DecisionBackend, DecisionRequest, Receipt};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Answers every noul with 0.95 and every choice with the first option.
    struct MockBackend {
        requests: Mutex<Vec<DecisionRequest>>,
        noul: f64,
        choice: Option<String>,
    }

    impl MockBackend {
        fn new(noul: f64, choice: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                requests: Mutex::new(Vec::new()),
                noul,
                choice: choice.map(str::to_string),
            })
        }
    }

    impl DecisionBackend for MockBackend {
        fn decide<'a>(
            &'a self,
            req: &'a DecisionRequest,
            _deadline: Duration,
            _abort: &'a AbortController,
        ) -> crate::services::decision::BackendFuture<'a> {
            self.requests.lock().unwrap().push(req.clone());
            let mut answers = BTreeMap::new();
            for (key, question) in &req.questions {
                let answer = match question {
                    Question::Noul { .. } => Answer::Noul {
                        probability: self.noul,
                    },
                    Question::Choice { options, .. } => Answer::Choice {
                        choice: self
                            .choice
                            .clone()
                            .or_else(|| options.keys().next().cloned())
                            .unwrap_or_default(),
                        probabilities: BTreeMap::new(),
                        confidence: 0.9,
                    },
                    Question::Score { .. } => Answer::Score {
                        level: 0,
                        confidence: 0.9,
                    },
                };
                answers.insert(key.clone(), answer);
            }
            let receipt = Receipt {
                point: req.point.as_str().to_string(),
                backend: "mock".into(),
                model: "mock-model".into(),
                answers: answers.clone(),
                state: crate::services::decision::digest_state(&req.state),
                ..Default::default()
            };
            Box::pin(async move { DecisionOutcome::Answered { answers, receipt } })
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    struct Fixture {
        config_dir: PathBuf,
        memory_dir: PathBuf,
        _lock: crate::utils::test_env::TestEnvGuard<'static>,
        _env: Vec<crate::utils::test_env::EnvVarGuard>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            test_override::clear();
            let _ = std::fs::remove_dir_all(&self.config_dir);
            let _ = std::fs::remove_dir_all(&self.memory_dir);
        }
    }

    fn fixture(switches: crate::utils::settings::nyan::MemoryDecisionShadowSettings) -> Fixture {
        use crate::utils::settings::nyan::{DecisionSettings, MemorySettings, NyanSettings};
        use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

        let lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root =
            crate::utils::node_os::tmpdir().join(format!("nyan-memdp-{}", uuid::Uuid::new_v4()));
        let memory_dir = root.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let env = vec![
            EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_dir),
            EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &memory_dir),
        ];
        let memory = MemorySettings {
            decision_shadow: Some(switches),
            ..Default::default()
        };
        crate::memdir::paths::seed_settings(
            crate::utils::settings::types::SettingsJson {
                auto_memory_enabled: Some(true),
                nyan: Some(Box::new(NyanSettings {
                    memory: Some(memory),
                    decision: Some(DecisionSettings::default()),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        Fixture {
            config_dir,
            memory_dir,
            _lock: lock,
            _env: env,
        }
    }

    fn all_on() -> crate::utils::settings::nyan::MemoryDecisionShadowSettings {
        crate::utils::settings::nyan::MemoryDecisionShadowSettings {
            extract_now: Some(true),
            extract_skip: Some(true),
            memory_type: Some(true),
            duplicate: Some(true),
            consolidate: Some(true),
        }
    }

    fn install(backend: Arc<MockBackend>) -> Arc<DecisionProvider> {
        let settings = crate::utils::settings::nyan::DecisionSettings {
            enabled: Some(true),
            ..Default::default()
        };
        let provider = Arc::new(DecisionProvider::with_backend(settings, backend));
        test_override::set(provider.clone());
        provider
    }

    fn read_log(config_dir: &Path, suffix: &str) -> Vec<MemoryShadowRecord> {
        let path = config_dir
            .join("decision")
            .join(format!("memory.{suffix}.jsonl"));
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        content
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn message(text: &str) -> Message {
        Message::User(crate::utils::messages::create_user_message(
            text.to_string(),
        ))
    }

    #[test]
    fn excerpt_finds_the_user_prompt_past_tool_results() {
        let mut messages = vec![message("I use the fish shell and want terse replies")];
        for index in 0..8 {
            messages.push(Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::ToolResult(
                    crate::types::message::ToolResult {
                        tool_use_id: crate::types::ids::ToolUseId(format!("toolu_{index}")),
                        content: "tool output".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: None,
                    },
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
            }));
        }
        messages.push(Message::Assistant(
            crate::types::message::AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::AssistantContent::Text(
                    "OK".to_string(),
                )],
                model: None,
                stop_reason: None,
                usage: None,
            },
        ));
        let excerpt = turn_excerpt(&messages, 6_000);
        assert!(excerpt.contains("fish shell"), "{excerpt}");
        assert!(excerpt.contains("assistant: OK"), "{excerpt}");
    }

    #[test]
    fn every_switch_is_independent_and_off_by_default() {
        let default = crate::utils::settings::nyan::MemorySettings::default();
        for point in MEMORY_POINTS {
            assert!(!point_switch(*point, &default), "{point:?} defaults off");
            assert!(log_suffix(*point).is_some());
        }
    }

    #[tokio::test]
    async fn extract_now_records_model_and_rule_agreement() {
        let fixture = fixture(all_on());
        let backend = MockBackend::new(0.95, None);
        install(backend.clone());
        let record = evaluate_extract_now(&[message("I prefer fish shell")], false, 3, 1_000)
            .await
            .expect("enabled");
        assert_eq!(record.point, "memory.extract_now");
        assert_eq!(record.outcome, "shadow");
        assert!(!record.applied);
        assert_eq!(record.detail["modelSaysExtractNow"], true);
        assert_eq!(record.detail["deterministicDue"], false);
        assert_eq!(record.detail["agrees"], false);
        assert!(!record.state_sha256.is_empty());
        let logged = read_log(&fixture.config_dir, "extract_now");
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].detail, record.detail);
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_disabled_switch_makes_no_request_and_writes_nothing() {
        let fixture = fixture(Default::default());
        let backend = MockBackend::new(0.95, None);
        install(backend.clone());
        assert!(
            evaluate_extract_now(&[message("x")], true, 8, 30_000)
                .await
                .is_none()
        );
        assert!(
            evaluate_extract_skip(&[message("x")], 8, 30_000)
                .await
                .is_none()
        );
        assert!(evaluate_types(&[], 30).await.is_none());
        assert!(
            evaluate_consolidate(ConsolidationContext {
                last_start_ms: None,
                newest_file_ms: None,
                new_file_count: 9,
                min_hours: 24,
                min_new_files: 5,
                now_ms: 1,
                deterministic_due: true,
            })
            .await
            .is_none()
        );
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(!fixture.config_dir.join("decision").exists());
    }

    #[tokio::test]
    async fn type_and_duplicate_compare_against_the_extraction_agent() {
        let fixture = fixture(all_on());
        install(MockBackend::new(0.1, Some("feedback")));
        let before = vec![MemoryFileSnapshot {
            path: fixture.memory_dir.join("a.md"),
            name: "a".into(),
            description: Some("old".into()),
            memory_type: Some("feedback".into()),
            body_excerpt: "old body".into(),
        }];
        std::fs::write(fixture.memory_dir.join("a.md"), "new body").unwrap();
        let after = snapshot_memory_files();
        let changed = changed_since(&before, &after);
        assert_eq!(changed.len(), 1);

        let type_record = evaluate_types(&changed, 30).await.unwrap();
        assert_eq!(type_record.point, "memory.type");
        let entry = &type_record.detail["entries"][0];
        assert_eq!(entry["modelType"], "feedback");
        assert_eq!(entry["deterministicType"], serde_json::Value::Null);
        let duplicate_record = evaluate_duplicates(&before, &changed, 30, 30)
            .await
            .unwrap();
        assert_eq!(duplicate_record.point, "memory.duplicate");
        let entry = &duplicate_record.detail["entries"][0];
        assert_eq!(entry["deterministicDuplicate"], true);
        assert_eq!(entry["modelSaysDuplicate"], false);
        assert_eq!(entry["agrees"], false);
        assert_eq!(read_log(&fixture.config_dir, "type").len(), 1);
        assert_eq!(read_log(&fixture.config_dir, "duplicate").len(), 1);
    }

    #[tokio::test]
    async fn consolidate_compares_with_the_24h_rule() {
        let fixture = fixture(all_on());
        install(MockBackend::new(0.9, None));
        let record = evaluate_consolidate(ConsolidationContext {
            last_start_ms: Some(0),
            newest_file_ms: Some(1),
            new_file_count: 3,
            min_hours: 24,
            min_new_files: 5,
            now_ms: 13 * 3_600_000,
            deterministic_due: false,
        })
        .await
        .unwrap();
        assert_eq!(record.point, "memory.consolidate");
        assert_eq!(record.detail["deterministicDue"], false);
        assert_eq!(record.detail["modelSaysConsolidate"], true);
        assert_eq!(record.detail["agrees"], false);
        assert!(
            (record.detail["hoursSinceLastConsolidation"]
                .as_f64()
                .unwrap()
                - 13.0)
                .abs()
                < 1e-6
        );
        assert_eq!(read_log(&fixture.config_dir, "consolidate").len(), 1);
    }

    #[tokio::test]
    async fn a_backend_timeout_is_a_silent_recorded_fallback() {
        struct HangingBackend;
        impl DecisionBackend for HangingBackend {
            fn decide<'a>(
                &'a self,
                _req: &'a DecisionRequest,
                _deadline: Duration,
                _abort: &'a AbortController,
            ) -> crate::services::decision::BackendFuture<'a> {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    unreachable!()
                })
            }
            fn name(&self) -> &'static str {
                "hanging"
            }
        }

        let fixture = fixture(all_on());
        let settings = crate::utils::settings::nyan::DecisionSettings {
            enabled: Some(true),
            deadline_ms: Some(crate::utils::settings::nyan::DecisionDeadlineSettings {
                critical: None,
                background: Some(120),
            }),
            ..Default::default()
        };
        test_override::set(Arc::new(DecisionProvider::with_backend(
            settings,
            Arc::new(HangingBackend),
        )));
        let record = evaluate_extract_now(&[message("x")], true, 8, 30_000)
            .await
            .expect("a timed-out evaluation still records");
        assert_eq!(record.outcome, "fallback");
        assert_eq!(record.fallback_reason.as_deref(), Some("timeout"));
        assert_eq!(read_log(&fixture.config_dir, "extract_now").len(), 1);
    }

    #[test]
    fn changed_since_sees_new_and_rewritten_files_only() {
        let base = |path: &str, body: &str| MemoryFileSnapshot {
            path: PathBuf::from(path),
            name: "n".into(),
            description: None,
            memory_type: Some("project".into()),
            body_excerpt: body.into(),
        };
        let before = vec![base("/m/a.md", "a"), base("/m/b.md", "b")];
        let after = vec![
            base("/m/a.md", "a"),
            base("/m/b.md", "b2"),
            base("/m/c.md", "c"),
        ];
        let changed = changed_since(&before, &after);
        let names: Vec<_> = changed
            .iter()
            .map(|file| file.path.display().to_string())
            .collect();
        assert_eq!(names, vec!["/m/b.md", "/m/c.md"]);
    }

    #[test]
    fn cached_provider_is_reused_until_settings_change() {
        use crate::utils::settings::nyan::DecisionSettings;
        let a = DecisionSettings::default();
        let first = cached_provider(a.clone());
        assert!(Arc::ptr_eq(&first, &cached_provider(a.clone())));
        let b = DecisionSettings {
            enabled: Some(!a.enabled()),
            ..a.clone()
        };
        let third = cached_provider(b.clone());
        assert!(!Arc::ptr_eq(&first, &third));
        assert!(Arc::ptr_eq(&third, &cached_provider(b)));
    }
}
