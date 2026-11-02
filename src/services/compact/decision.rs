//! @nyan: Decision Provider seam for `compaction.keep` shadow mode
//! (compaction C14, decision-provider 4.4, SMOKE-EVAL-2026-11-01 C2).
//!
//! # Objective
//! During pre-compaction masking (`mask_tool_results_if_needed`), when
//! `compaction.keep` shadow mode is enabled, evaluate which candidate tool
//! results the keep policy would preserve vs clear, compare against the actual
//! C2 ordering (the status quo), and append a structured record to
//! `$CLAUDE_CONFIG_DIR/decision/compaction.keep.jsonl`.
//!
//! # Contract
//! 1. In shadow mode, the actual masking result remains 100% unchanged.
//! 2. Keep policy semantic (C14):
//!    - Candidates judged "still needed" are moved to the end of the clearing
//!      sequence.
//!    - Clearing starts from "not needed" candidates (oldest first) up to
//!      the required tokens (`measured_before - target`).
//!    - Any "not needed" candidate after the earliest cleared position is also
//!      cleared.
//!    - "Still needed" candidates are only cleared if "not needed" candidates
//!      were insufficient to satisfy the required reduction. They are NOT
//!      cleared merely for appearing after the earliest cleared position.
//! 3. When disabled (default), no decision query is made, no log is written,
//!    and execution matches master exactly.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::services::decision::{
    Answer, DecisionOutcome, DecisionPoint, DecisionProvider, DeterministicAnswer, Question,
    SafeSet,
};
use crate::tool::ToolUseContext;
use crate::types::message::{Message, UserContent};

/// Candidate information passed into keep-shadow evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskCandidateInfo {
    pub message_index: usize,
    pub tool_use_id: String,
    pub tool_name: String,
    pub tokens: i64,
    pub bytes: usize,
    pub preview: String,
}

/// Result of ordering candidates under the status quo (C2 deterministic rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualOrderingResult {
    pub cleared_ids: Vec<String>,
    pub preserved_ids: Vec<String>,
    pub cleared_tokens: i64,
    pub earliest_cleared_index: Option<usize>,
}

/// Result of ordering candidates under keep policy (C14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepOrderingResult {
    pub cleared_ids: Vec<String>,
    pub preserved_ids: Vec<String>,
    pub cleared_tokens: i64,
    pub earliest_cleared_index: Option<usize>,
}

/// Comparison per candidate between keep policy and actual masking.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateComparison {
    pub tool_use_id: String,
    pub tool_name: String,
    pub tokens: i64,
    pub bytes: usize,
    pub keep_decision: bool,
    pub keep_probability: f64,
    pub keep_confidence: f64,
    pub action_keep: String,   // "clear" or "preserve"
    pub action_actual: String, // "clear" or "preserve"
}

/// Structured shadow record appended to `$CLAUDE_CONFIG_DIR/decision/compaction.keep.jsonl`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionKeepShadowRecord {
    pub timestamp: String,
    pub point: String,
    pub model: String,
    pub latency_ms: u64,
    pub mode: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub timeout_ms: u64,
    pub measured_before: i64,
    pub threshold: i64,
    pub target: i64,
    pub required_tokens: i64,
    pub candidates_count: usize,
    pub candidates: Vec<CandidateComparison>,
    pub actual_cleared_count: usize,
    pub actual_cleared_tokens: i64,
    pub keep_cleared_count: usize,
    pub keep_cleared_tokens: i64,
    pub agreed_count: usize,
    pub differed_count: usize,
    pub keep_would_preserve_actual_cleared: Vec<String>,
    pub keep_would_clear_actual_preserved: Vec<String>,
    pub earliest_cleared_actual: Option<usize>,
    pub earliest_cleared_keep: Option<usize>,
    pub cache_prefix_saved_steps: i64,
}

/// Checks whether `compaction.keep` shadow mode is enabled.
/// Default: false (closed).
pub fn is_keep_shadow_enabled() -> bool {
    #[cfg(test)]
    if test_override::get().is_some() {
        return true;
    }
    let settings = crate::utils::settings::get_initial_settings();
    let nyan = match settings.nyan.as_ref() {
        Some(nyan) => nyan,
        None => return false,
    };
    if nyan.compaction().keep_shadow() {
        return true;
    }
    // Also respect explicit point configuration if user configured it
    if let Some(decision) = &nyan.decision
        && decision.enabled()
        && let Some(points) = &decision.points
        && let Some(point_cfg) = points.get("compaction.keep")
        && (point_cfg.mode == Some(crate::utils::settings::nyan::DecisionMode::Shadow)
            || point_cfg.mode == Some(crate::utils::settings::nyan::DecisionMode::Active))
    {
        return true;
    }
    false
}

/// Target path for compaction.keep shadow logs.
pub fn shadow_log_path() -> PathBuf {
    crate::utils::env_utils::get_claude_config_home_dir().join("decision/compaction.keep.jsonl")
}

/// Extracts a representative task excerpt/summary from recent conversation messages.
pub fn extract_current_task(messages: &[Message]) -> String {
    // Look backwards for the most recent user prompt that has text content
    for message in messages.iter().rev() {
        if let Message::User(user) = message {
            for content in &user.content {
                if let UserContent::Text(text) = content {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.chars().take(1500).collect();
                    }
                }
            }
        }
    }
    // Fallback: earliest user text
    for message in messages {
        if let Message::User(user) = message {
            for content in &user.content {
                if let UserContent::Text(text) = content {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.chars().take(1500).collect();
                    }
                }
            }
        }
    }
    "General task context".to_string()
}

/// Computes candidate clearing under actual status quo (C2 rule).
pub fn compute_actual_ordering(
    candidates: &[MaskCandidateInfo],
    target: i64,
    measured_before: i64,
) -> ActualOrderingResult {
    let required = measured_before.saturating_sub(target).max(1);
    let mut saved = 0_i64;
    let mut first_selected = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        if saved >= required {
            break;
        }
        saved = saved.saturating_add(candidate.tokens);
        first_selected = index;
    }

    let cleared: Vec<_> = candidates.iter().skip(first_selected).cloned().collect();
    let preserved: Vec<_> = candidates.iter().take(first_selected).cloned().collect();
    let cleared_tokens = cleared.iter().map(|c| c.tokens).sum();
    let earliest_cleared_index = if cleared.is_empty() {
        None
    } else {
        Some(first_selected)
    };

    ActualOrderingResult {
        cleared_ids: cleared.into_iter().map(|c| c.tool_use_id).collect(),
        preserved_ids: preserved.into_iter().map(|c| c.tool_use_id).collect(),
        cleared_tokens,
        earliest_cleared_index,
    }
}

/// Computes candidate clearing under keep policy (C14 rule).
pub fn compute_keep_ordering(
    candidates: &[MaskCandidateInfo],
    keep_judgments: &HashMap<String, bool>,
    target: i64,
    measured_before: i64,
) -> KeepOrderingResult {
    let required = measured_before.saturating_sub(target).max(1);

    let mut not_needed: Vec<(usize, &MaskCandidateInfo)> = Vec::new();
    let mut needed: Vec<(usize, &MaskCandidateInfo)> = Vec::new();

    for (index, candidate) in candidates.iter().enumerate() {
        let is_needed = keep_judgments
            .get(&candidate.tool_use_id)
            .copied()
            .unwrap_or(false);
        if is_needed {
            needed.push((index, candidate));
        } else {
            not_needed.push((index, candidate));
        }
    }

    let mut saved = 0_i64;
    let mut cleared_indices = HashSet::new();

    // 1. Accumulate oldest-first from not_needed
    let mut first_not_needed_cleared_seq_idx = None;
    for (seq_idx, (_orig_idx, candidate)) in not_needed.iter().enumerate() {
        if saved >= required {
            break;
        }
        saved = saved.saturating_add(candidate.tokens);
        if first_not_needed_cleared_seq_idx.is_none() {
            first_not_needed_cleared_seq_idx = Some(seq_idx);
        }
    }

    // Apply "not_needed candidates starting from earliest cleared are all cleared"
    if let Some(start_seq) = first_not_needed_cleared_seq_idx {
        for (orig_idx, _) in not_needed.iter().skip(start_seq) {
            cleared_indices.insert(*orig_idx);
        }
    }

    // 2. If saved < required, clear from needed (oldest-first)
    // Only candidates needed to reach target are cleared.
    if saved < required {
        for (orig_idx, candidate) in needed.iter() {
            if saved >= required {
                break;
            }
            saved = saved.saturating_add(candidate.tokens);
            cleared_indices.insert(*orig_idx);
        }
    }

    let mut cleared_ids = Vec::new();
    let mut preserved_ids = Vec::new();
    let mut cleared_tokens = 0_i64;
    let mut earliest_cleared_index = None;

    for (index, candidate) in candidates.iter().enumerate() {
        if cleared_indices.contains(&index) {
            if earliest_cleared_index.is_none() {
                earliest_cleared_index = Some(index);
            }
            cleared_tokens = cleared_tokens.saturating_add(candidate.tokens);
            cleared_ids.push(candidate.tool_use_id.clone());
        } else {
            preserved_ids.push(candidate.tool_use_id.clone());
        }
    }

    KeepOrderingResult {
        cleared_ids,
        preserved_ids,
        cleared_tokens,
        earliest_cleared_index,
    }
}

/// Writes a shadow record as one line in JSONL format.
pub fn write_shadow_record(record: &CompactionKeepShadowRecord) -> std::io::Result<()> {
    let path = shadow_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    use std::io::Write;
    writeln!(file, "{}", line)?;
    Ok(())
}

/// Maximum concurrent compaction.keep shadow evaluations allowed in flight.
pub const MAX_CONCURRENT_SHADOW_TASKS: usize = 1;

static IN_FLIGHT_SHADOW_TASKS: AtomicUsize = AtomicUsize::new(0);
static SKIPPED_SHADOW_TASKS: AtomicUsize = AtomicUsize::new(0);

/// Current number of in-flight shadow evaluation tasks.
pub fn in_flight_shadow_tasks_count() -> usize {
    IN_FLIGHT_SHADOW_TASKS.load(Ordering::SeqCst)
}

/// Number of shadow evaluations skipped due to reaching the concurrency limit.
pub fn skipped_shadow_tasks_count() -> usize {
    SKIPPED_SHADOW_TASKS.load(Ordering::SeqCst)
}

#[cfg(test)]
pub fn reset_shadow_task_counters_for_test() {
    IN_FLIGHT_SHADOW_TASKS.store(0, Ordering::SeqCst);
    SKIPPED_SHADOW_TASKS.store(0, Ordering::SeqCst);
}

fn get_or_init_shadow_runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("nyan-keep-shadow")
                .enable_all()
                .build()
            {
                Ok(rt) => Some(rt),
                Err(err) => {
                    tracing::error!(
                        target: "nyan::compact",
                        error = %err,
                        "failed to build compaction.keep shadow runtime"
                    );
                    crate::utils::debug::log_for_debugging(&format!(
                        "compaction.keep: failed to build shadow runtime: {err}"
                    ));
                    None
                }
            }
        })
        .as_ref()
}

struct InFlightGuard;

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let _ = IN_FLIGHT_SHADOW_TASKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |val| {
            Some(val.saturating_sub(1))
        });
    }
}

/// Spawns compaction.keep shadow evaluation onto a shared background runtime
/// without blocking the caller or propagating panics. Bounds concurrency to
/// [`MAX_CONCURRENT_SHADOW_TASKS`]; excess requests are skipped and counted.
pub fn spawn_compaction_keep_shadow(
    messages: Vec<Message>,
    candidates: Vec<MaskCandidateInfo>,
    target: i64,
    measured_before: i64,
    threshold: i64,
    context: ToolUseContext,
) {
    if !is_keep_shadow_enabled() || candidates.is_empty() {
        return;
    }

    if threshold <= 0 {
        tracing::warn!(
            target: "nyan::compact",
            threshold,
            "compaction.keep shadow skipped: threshold is 0 (degenerate configuration)"
        );
        crate::utils::debug::log_for_debugging(
            "spawn_compaction_keep_shadow: threshold is 0 (degenerate configuration), skipping shadow task",
        );
        return;
    }

    let acquired = IN_FLIGHT_SHADOW_TASKS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            if current >= MAX_CONCURRENT_SHADOW_TASKS {
                None
            } else {
                Some(current + 1)
            }
        })
        .is_ok();

    if !acquired {
        SKIPPED_SHADOW_TASKS.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            target: "nyan::compact",
            "compaction.keep shadow task skipped: concurrency limit reached"
        );
        crate::utils::debug::log_for_debugging(
            "spawn_compaction_keep_shadow: concurrency limit reached, skipping shadow evaluation",
        );
        return;
    }

    let Some(runtime) = get_or_init_shadow_runtime() else {
        let _ = IN_FLIGHT_SHADOW_TASKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |val| {
            Some(val.saturating_sub(1))
        });
        return;
    };

    runtime.spawn(async move {
        let _guard = InFlightGuard;
        let task = std::panic::AssertUnwindSafe(async move {
            evaluate_compaction_keep_shadow_inner(
                &messages,
                &candidates,
                target,
                measured_before,
                threshold,
                &context,
            )
            .await;
        });
        if let Err(panic_payload) = futures::FutureExt::catch_unwind(task).await {
            let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                *s
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.as_str()
            } else {
                "unknown panic"
            };
            tracing::error!(
                target: "nyan::compact",
                panic = msg,
                "compaction.keep shadow task panicked, caught safely"
            );
            crate::utils::debug::log_for_debugging(&format!(
                "compaction.keep shadow task panicked: {msg}"
            ));
        }
    });
}

/// Evaluates compaction.keep shadow mode in a boxed future so the query loop
/// does not inline the decision provider's state machine into its own future.
pub fn evaluate_compaction_keep_shadow<'a>(
    messages: &'a [Message],
    candidates: &'a [MaskCandidateInfo],
    target: i64,
    measured_before: i64,
    threshold: i64,
    context: &'a ToolUseContext,
) -> Pin<Box<dyn Future<Output = Option<CompactionKeepShadowRecord>> + Send + 'a>> {
    Box::pin(evaluate_compaction_keep_shadow_inner(
        messages,
        candidates,
        target,
        measured_before,
        threshold,
        context,
    ))
}

/// Evaluates compaction.keep shadow mode: asks DecisionProvider (or mock),
/// compares keep ordering with actual ordering, logs to JSONL, and returns the record.
async fn evaluate_compaction_keep_shadow_inner(
    messages: &[Message],
    candidates: &[MaskCandidateInfo],
    target: i64,
    measured_before: i64,
    threshold: i64,
    context: &ToolUseContext,
) -> Option<CompactionKeepShadowRecord> {
    if candidates.is_empty() {
        return None;
    }

    if threshold <= 0 {
        tracing::warn!(
            target: "nyan::compact",
            threshold,
            "compaction.keep shadow skipped: threshold is 0 (degenerate configuration)"
        );
        crate::utils::debug::log_for_debugging(
            "evaluate_compaction_keep_shadow: threshold is 0 (degenerate configuration), skipping shadow record",
        );
        return None;
    }

    let provider = {
        #[cfg(test)]
        if let Some(p) = test_override::get() {
            p
        } else {
            std::sync::Arc::new(DecisionProvider::from_nyan(
                &crate::utils::settings::get_initial_settings()
                    .nyan
                    .unwrap_or_default(),
            ))
        }
        #[cfg(not(test))]
        std::sync::Arc::new(DecisionProvider::from_nyan(
            &crate::utils::settings::get_initial_settings()
                .nyan
                .unwrap_or_default(),
        ))
    };

    let deadline = provider.settings().deadline(
        crate::utils::settings::nyan::DecisionSettings::is_critical_point(
            DecisionPoint::CompactionKeep.as_str(),
        ),
    );
    let timeout_ms = deadline.as_millis() as u64;

    let start_time = Instant::now();
    let current_task = extract_current_task(messages);

    // decision-provider.md §3: 单次最多 30 个候选，超出按排名截取前 30
    let max_batch = (provider.settings().max_batch() as usize).min(30);
    let judged_candidates = if candidates.len() > max_batch {
        &candidates[..max_batch]
    } else {
        candidates
    };

    let candidate_previews: Vec<_> = judged_candidates
        .iter()
        .map(|c| {
            json!({
                "id": c.tool_use_id,
                "tool": c.tool_name,
                "tokens": c.tokens,
                "bytes": c.bytes,
                "preview": c.preview,
            })
        })
        .collect();

    let state = json!({
        "task": current_task,
        "candidates": candidate_previews,
    });

    let mut questions = BTreeMap::new();
    let mut deterministic = BTreeMap::new();
    for candidate in judged_candidates {
        questions.insert(
            candidate.tool_use_id.clone(),
            Question::noul(format!(
                "Is the output of tool '{}' (id {}) still needed for the current task?",
                candidate.tool_name, candidate.tool_use_id
            )),
        );
        // Deterministic answer: C2 does not single out any candidate to keep
        deterministic.insert(
            candidate.tool_use_id.clone(),
            DeterministicAnswer::Noul(false),
        );
    }

    let abort = &context.abort_controller;

    let outcome = provider
        .decide(
            DecisionPoint::CompactionKeep,
            state,
            questions,
            &deterministic,
            &SafeSet::any(),
            abort,
        )
        .await;

    let latency_ms = start_time.elapsed().as_millis() as u64;

    let mut keep_judgments = HashMap::new();
    let mut probabilities = HashMap::new();
    let mut confidences = HashMap::new();

    let (outcome_str, fallback_reason, model_name) = match &outcome {
        DecisionOutcome::Answered { answers, receipt } => {
            for (id, answer) in answers {
                if let Answer::Noul { probability } = answer {
                    keep_judgments.insert(id.clone(), *probability >= 0.5);
                    probabilities.insert(id.clone(), *probability);
                    confidences.insert(id.clone(), answer.confidence());
                }
            }
            ("answered", None, receipt.model.clone())
        }
        DecisionOutcome::Fallback { reason, receipt } => {
            // In shadow mode, receipt.answers still contains the model's judgment!
            for (id, answer) in &receipt.answers {
                if let Answer::Noul { probability } = answer {
                    keep_judgments.insert(id.clone(), *probability >= 0.5);
                    probabilities.insert(id.clone(), *probability);
                    confidences.insert(id.clone(), answer.confidence());
                }
            }
            let outcome_str = if !receipt.answers.is_empty() {
                "answered"
            } else {
                "fallback"
            };
            (
                outcome_str,
                Some(reason.as_str().to_string()),
                receipt.model.clone(),
            )
        }
    };

    let actual_ordering = compute_actual_ordering(candidates, target, measured_before);
    let keep_ordering = compute_keep_ordering(candidates, &keep_judgments, target, measured_before);

    let actual_cleared_set: HashSet<_> = actual_ordering.cleared_ids.iter().cloned().collect();
    let keep_cleared_set: HashSet<_> = keep_ordering.cleared_ids.iter().cloned().collect();

    let mut comparisons = Vec::new();
    let mut agreed_count = 0;
    let mut differed_count = 0;
    let mut keep_would_preserve_actual_cleared = Vec::new();
    let mut keep_would_clear_actual_preserved = Vec::new();

    for candidate in candidates {
        let is_keep = keep_judgments
            .get(&candidate.tool_use_id)
            .copied()
            .unwrap_or(false);
        let prob = probabilities
            .get(&candidate.tool_use_id)
            .copied()
            .unwrap_or(0.0);
        let conf = confidences
            .get(&candidate.tool_use_id)
            .copied()
            .unwrap_or(0.0);

        let in_actual_cleared = actual_cleared_set.contains(&candidate.tool_use_id);
        let in_keep_cleared = keep_cleared_set.contains(&candidate.tool_use_id);

        let action_actual = if in_actual_cleared {
            "clear"
        } else {
            "preserve"
        };
        let action_keep = if in_keep_cleared { "clear" } else { "preserve" };

        if in_actual_cleared == in_keep_cleared {
            agreed_count += 1;
        } else {
            differed_count += 1;
            if in_actual_cleared && !in_keep_cleared {
                keep_would_preserve_actual_cleared.push(candidate.tool_use_id.clone());
            } else if !in_actual_cleared && in_keep_cleared {
                keep_would_clear_actual_preserved.push(candidate.tool_use_id.clone());
            }
        }

        comparisons.push(CandidateComparison {
            tool_use_id: candidate.tool_use_id.clone(),
            tool_name: candidate.tool_name.clone(),
            tokens: candidate.tokens,
            bytes: candidate.bytes,
            keep_decision: is_keep,
            keep_probability: prob,
            keep_confidence: conf,
            action_keep: action_keep.to_string(),
            action_actual: action_actual.to_string(),
        });
    }

    let cache_prefix_saved_steps = match (
        keep_ordering.earliest_cleared_index,
        actual_ordering.earliest_cleared_index,
    ) {
        (Some(k), Some(a)) => (k as i64) - (a as i64),
        _ => 0,
    };

    let record = CompactionKeepShadowRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        point: DecisionPoint::CompactionKeep.as_str().to_string(),
        model: if model_name.is_empty() {
            "typesafe/jev-1.14.0".to_string()
        } else {
            model_name
        },
        latency_ms,
        mode: "shadow".to_string(),
        outcome: outcome_str.to_string(),
        fallback_reason,
        timeout_ms,
        measured_before,
        threshold,
        target,
        required_tokens: measured_before.saturating_sub(target).max(1),
        candidates_count: candidates.len(),
        candidates: comparisons,
        actual_cleared_count: actual_ordering.cleared_ids.len(),
        actual_cleared_tokens: actual_ordering.cleared_tokens,
        keep_cleared_count: keep_ordering.cleared_ids.len(),
        keep_cleared_tokens: keep_ordering.cleared_tokens,
        agreed_count,
        differed_count,
        keep_would_preserve_actual_cleared,
        keep_would_clear_actual_preserved,
        earliest_cleared_actual: actual_ordering.earliest_cleared_index,
        earliest_cleared_keep: keep_ordering.earliest_cleared_index,
        cache_prefix_saved_steps,
    };

    if let Err(err) = write_shadow_record(&record) {
        crate::utils::debug::log_for_debugging(&format!(
            "evaluate_compaction_keep_shadow: failed to write shadow log: {err}"
        ));
    }

    Some(record)
}

#[cfg(test)]
pub(crate) mod test_override {
    use crate::services::decision::DecisionProvider;
    use std::sync::{Mutex, OnceLock};

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

    fn sample_candidates() -> Vec<MaskCandidateInfo> {
        vec![
            MaskCandidateInfo {
                message_index: 2,
                tool_use_id: "toolu_01".into(),
                tool_name: "Bash".into(),
                tokens: 3000,
                bytes: 12000,
                preview: "cargo test output...".into(),
            },
            MaskCandidateInfo {
                message_index: 4,
                tool_use_id: "toolu_02".into(),
                tool_name: "Read".into(),
                tokens: 4000,
                bytes: 16000,
                preview: "file content src/main.rs...".into(),
            },
            MaskCandidateInfo {
                message_index: 6,
                tool_use_id: "toolu_03".into(),
                tool_name: "Bash".into(),
                tokens: 2000,
                bytes: 8000,
                preview: "git status...".into(),
            },
        ]
    }

    #[test]
    fn compute_actual_ordering_clears_from_first_selected_onwards() {
        let candidates = sample_candidates();
        // measured_before = 10000, target = 6000 -> required = 4000
        // toolu_01 (3000) -> saved 3000 < 4000
        // toolu_02 (4000) -> saved 7000 >= 4000, index = 1
        // actual clears from index 1 onwards: toolu_02, toolu_03 (skip 1)
        let actual = compute_actual_ordering(&candidates, 6000, 10000);
        assert_eq!(actual.cleared_ids, vec!["toolu_02", "toolu_03"]);
        assert_eq!(actual.preserved_ids, vec!["toolu_01"]);
        assert_eq!(actual.cleared_tokens, 6000);
        assert_eq!(actual.earliest_cleared_index, Some(1));
    }

    #[test]
    fn keep_ordering_preserves_needed_and_clears_not_needed() {
        let candidates = sample_candidates();
        let mut judgments = HashMap::new();
        // toolu_02 is needed (e.g. key read of main.rs), others are not needed
        judgments.insert("toolu_01".into(), false);
        judgments.insert("toolu_02".into(), true);
        judgments.insert("toolu_03".into(), false);

        // required = 10000 - 6000 = 4000
        // not_needed: [toolu_01 (3000), toolu_03 (2000)]
        // toolu_01 + toolu_03 = 5000 >= 4000.
        // not_needed clears toolu_01 and toolu_03!
        // needed toolu_02 is PRESERVED!
        let keep = compute_keep_ordering(&candidates, &judgments, 6000, 10000);
        assert_eq!(keep.cleared_ids, vec!["toolu_01", "toolu_03"]);
        assert_eq!(keep.preserved_ids, vec!["toolu_02"]);
        assert_eq!(keep.cleared_tokens, 5000);
        assert_eq!(keep.earliest_cleared_index, Some(0));
    }

    #[test]
    fn keep_ordering_clears_needed_only_when_not_needed_insufficient() {
        let candidates = sample_candidates();
        let mut judgments = HashMap::new();
        judgments.insert("toolu_01".into(), false);
        judgments.insert("toolu_02".into(), true);
        judgments.insert("toolu_03".into(), true);

        // required = 10000 - 4000 = 6000
        // not_needed: [toolu_01 (3000)] -> saved 3000 < 6000
        // needed: [toolu_02 (4000), toolu_03 (2000)]
        // toolu_01 cleared (3000). Need 3000 more:
        // toolu_02 cleared (+4000 -> 7000 >= 6000).
        // toolu_03 is NOT cleared (needed and not required)!
        let keep = compute_keep_ordering(&candidates, &judgments, 4000, 10000);
        assert_eq!(keep.cleared_ids, vec!["toolu_01", "toolu_02"]);
        assert_eq!(keep.preserved_ids, vec!["toolu_03"]);
        assert_eq!(keep.cleared_tokens, 7000);
    }

    #[test]
    fn record_serialization_round_trip() {
        let record = CompactionKeepShadowRecord {
            timestamp: "2026-11-01T20:00:00Z".into(),
            point: "compaction.keep".into(),
            model: "typesafe/jev-1.14.0".into(),
            latency_ms: 450,
            mode: "shadow".into(),
            outcome: "answered".into(),
            fallback_reason: Some("shadow".into()),
            timeout_ms: 800,
            measured_before: 10000,
            threshold: 8000,
            target: 6000,
            required_tokens: 4000,
            candidates_count: 2,
            candidates: vec![
                CandidateComparison {
                    tool_use_id: "toolu_01".into(),
                    tool_name: "Bash".into(),
                    tokens: 3000,
                    bytes: 12000,
                    keep_decision: false,
                    keep_probability: 0.1,
                    keep_confidence: 0.8,
                    action_keep: "clear".into(),
                    action_actual: "preserve".into(),
                },
                CandidateComparison {
                    tool_use_id: "toolu_02".into(),
                    tool_name: "Read".into(),
                    tokens: 4000,
                    bytes: 16000,
                    keep_decision: true,
                    keep_probability: 0.9,
                    keep_confidence: 0.8,
                    action_keep: "preserve".into(),
                    action_actual: "clear".into(),
                },
            ],
            actual_cleared_count: 1,
            actual_cleared_tokens: 4000,
            keep_cleared_count: 1,
            keep_cleared_tokens: 3000,
            agreed_count: 0,
            differed_count: 2,
            keep_would_preserve_actual_cleared: vec!["toolu_02".into()],
            keep_would_clear_actual_preserved: vec!["toolu_01".into()],
            earliest_cleared_actual: Some(1),
            earliest_cleared_keep: Some(0),
            cache_prefix_saved_steps: -1,
        };

        let json_line = serde_json::to_string(&record).unwrap();
        let parsed: CompactionKeepShadowRecord = serde_json::from_str(&json_line).unwrap();
        assert_eq!(record, parsed);
        assert_eq!(parsed.mode, "shadow");
        assert_eq!(parsed.outcome, "answered");
        assert_eq!(parsed.timeout_ms, 800);
        assert_eq!(parsed.keep_would_preserve_actual_cleared, vec!["toolu_02"]);
    }

    #[tokio::test]
    async fn outcome_reflects_actual_result_answered_vs_timeout() {
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        use crate::services::decision::{
            Answer, BackendFuture, DecisionBackend, DecisionOutcome, DecisionProvider,
            DecisionRequest, FallbackReason, Receipt,
        };
        use std::sync::Arc;

        struct MockAnsweredBackend;
        impl DecisionBackend for MockAnsweredBackend {
            fn decide<'a>(
                &'a self,
                req: &'a DecisionRequest,
                _deadline: std::time::Duration,
                _abort: &'a crate::tool::AbortController,
            ) -> BackendFuture<'a> {
                let mut answers = BTreeMap::new();
                for q in req.questions.keys() {
                    answers.insert(q.clone(), Answer::Noul { probability: 0.9 });
                }
                Box::pin(async move {
                    DecisionOutcome::Fallback {
                        reason: FallbackReason::Shadow,
                        receipt: Receipt {
                            point: "compaction.keep".into(),
                            model: "typesafe/jev-1.14.0".into(),
                            backend: "mock".into(),
                            answers,
                            ..Default::default()
                        },
                    }
                })
            }
            fn name(&self) -> &'static str {
                "mock-answered"
            }
        }

        struct MockTimeoutBackend;
        impl DecisionBackend for MockTimeoutBackend {
            fn decide<'a>(
                &'a self,
                _req: &'a DecisionRequest,
                _deadline: std::time::Duration,
                _abort: &'a crate::tool::AbortController,
            ) -> BackendFuture<'a> {
                Box::pin(async move {
                    DecisionOutcome::Fallback {
                        reason: FallbackReason::Timeout,
                        receipt: Receipt {
                            point: "compaction.keep".into(),
                            model: "typesafe/jev-1.14.0".into(),
                            backend: "mock".into(),
                            ..Default::default()
                        },
                    }
                })
            }
            fn name(&self) -> &'static str {
                "mock-timeout"
            }
        }

        let candidates = sample_candidates();
        let context = ToolUseContext::default();
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "test task".to_string(),
        ))];

        // 1. Model answered in shadow mode -> outcome="answered", mode="shadow", fallbackReason="shadow"
        let mut decision_settings = crate::utils::settings::nyan::DecisionSettings::default();
        decision_settings.enabled = Some(true);
        let answered_provider = Arc::new(DecisionProvider::with_backend(
            decision_settings.clone(),
            Arc::new(MockAnsweredBackend),
        ));
        test_override::set(answered_provider);
        let record =
            evaluate_compaction_keep_shadow(&messages, &candidates, 6000, 10000, 8000, &context)
                .await
                .expect("should produce record");
        assert_eq!(record.mode, "shadow");
        assert_eq!(record.outcome, "answered");
        assert_eq!(record.fallback_reason, Some("shadow".into()));
        assert_eq!(record.timeout_ms, 800);

        // 2. Model timed out -> outcome="fallback", mode="shadow", fallbackReason="timeout"
        let timeout_provider = Arc::new(DecisionProvider::with_backend(
            decision_settings,
            Arc::new(MockTimeoutBackend),
        ));
        test_override::set(timeout_provider);
        let record =
            evaluate_compaction_keep_shadow(&messages, &candidates, 6000, 10000, 8000, &context)
                .await
                .expect("should produce record");
        assert_eq!(record.mode, "shadow");
        assert_eq!(record.outcome, "fallback");
        assert_eq!(record.fallback_reason, Some("timeout".into()));
        assert_eq!(record.timeout_ms, 800);

        test_override::clear();
    }

    #[tokio::test]
    async fn degenerate_threshold_zero_is_skipped() {
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let candidates = sample_candidates();
        let context = ToolUseContext::default();
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "test task".to_string(),
        ))];

        let record_zero =
            evaluate_compaction_keep_shadow(&messages, &candidates, 0, 10000, 0, &context).await;
        assert!(record_zero.is_none(), "threshold = 0 must be skipped");

        let record_neg =
            evaluate_compaction_keep_shadow(&messages, &candidates, 0, 10000, -500, &context).await;
        assert!(record_neg.is_none(), "threshold < 0 must be skipped");
    }

    #[test]
    fn concurrency_limit_skips_and_increments_counter() {
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        reset_shadow_task_counters_for_test();
        assert_eq!(in_flight_shadow_tasks_count(), 0);
        assert_eq!(skipped_shadow_tasks_count(), 0);

        let mut decision_settings = crate::utils::settings::nyan::DecisionSettings::default();
        decision_settings.enabled = Some(true);
        test_override::set(std::sync::Arc::new(
            crate::services::decision::DecisionProvider::new(decision_settings),
        ));

        // Simulate that maximum concurrent shadow tasks are already running
        IN_FLIGHT_SHADOW_TASKS.store(MAX_CONCURRENT_SHADOW_TASKS, Ordering::SeqCst);

        let candidates = sample_candidates();
        let context = ToolUseContext::default();
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "test task".to_string(),
        ))];

        // This call should be skipped immediately and increment skipped counter
        spawn_compaction_keep_shadow(messages, candidates, 6000, 10000, 8000, context);

        assert_eq!(skipped_shadow_tasks_count(), 1);
        assert_eq!(in_flight_shadow_tasks_count(), MAX_CONCURRENT_SHADOW_TASKS);

        // Reset
        reset_shadow_task_counters_for_test();
        test_override::clear();
    }

    #[tokio::test]
    async fn panic_in_shadow_task_does_not_propagate_or_leave_counter_dirty() {
        let _env_guard = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        use crate::services::decision::{
            BackendFuture, DecisionBackend, DecisionProvider, DecisionRequest,
        };
        use std::sync::Arc;

        struct PanicBackend;
        impl DecisionBackend for PanicBackend {
            fn decide<'a>(
                &'a self,
                _req: &'a DecisionRequest,
                _deadline: std::time::Duration,
                _abort: &'a crate::tool::AbortController,
            ) -> BackendFuture<'a> {
                panic!("deliberate panic inside decision backend");
            }
            fn name(&self) -> &'static str {
                "panic-backend"
            }
        }

        reset_shadow_task_counters_for_test();
        let mut decision_settings = crate::utils::settings::nyan::DecisionSettings::default();
        decision_settings.enabled = Some(true);
        let panic_provider = Arc::new(DecisionProvider::with_backend(
            decision_settings,
            Arc::new(PanicBackend),
        ));
        test_override::set(panic_provider);

        let candidates = sample_candidates();
        let context = ToolUseContext::default();
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "test task".to_string(),
        ))];

        // Should not panic the caller thread
        spawn_compaction_keep_shadow(messages, candidates, 6000, 10000, 8000, context);

        // Wait for background worker to catch the panic and run InFlightGuard drop
        let start = std::time::Instant::now();
        while in_flight_shadow_tasks_count() > 0
            && start.elapsed() < std::time::Duration::from_secs(2)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(
            in_flight_shadow_tasks_count(),
            0,
            "in-flight guard must clean up after panic"
        );
        test_override::clear();
    }
}
