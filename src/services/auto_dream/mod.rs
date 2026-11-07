//! Auto-dream memory consolidation service.
//!
//! @nyan: the service owns the lock, fork policy, and no-delete tool boundary.
//! [`run_consolidation`] takes the lock and runs one pass;
//! [`spawn_idle_consolidation_if_due`] is the turn-end trigger and
//! [`run_manual_consolidation`] backs `/memory` → "Consolidate now".

pub mod lock;

use crate::constants::query_source::QuerySource;
use crate::tool::{AbortController, ToolUseContext};
use crate::types::message::{AssistantContent, Message, UserContent, UserMessage};
use crate::utils::forked_agent::{CacheSafeParams, ForkedAgentParams, SubagentContextOverrides};
use lock::{ConsolidationLock, LockAcquireError};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolidationResult {
    pub suggested_deletions: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidationError {
    Busy(String),
    Fork(String),
    TimedOut,
    Aborted,
    MemoryGuardBlocked,
    Lock(String),
}

fn consolidation_prompt(
    index: &str,
    candidates: &[crate::memdir::memdir::MemoryCandidate],
    memory_dir: &Path,
) -> String {
    let files = candidates
        .iter()
        .map(|candidate| format!("- {}", candidate.path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[MEMORY CONSOLIDATION]\nRead the auto-memory files and MEMORY.md index. Deduplicate and update stale project/feedback/reference memories. You may write topic Markdown files and edit MEMORY.md only inside the auto-memory directory. You MUST NOT delete any file and you MUST NOT use Bash. Remove outdated entries from the index only when clearly obsolete, leave the file in place, and report every removed path as `SUGGEST_DELETE: <path>` for explicit user confirmation. Do not add `type: user` files to the index. Never save or expose secrets. The auto-memory directory is `{dir}`; always pass absolute paths inside it to every tool call (Glob/Grep need an explicit `path`).\n\nMEMORY.md:\n{index}\n\nUnindexed candidates:\n{files}",
        dir = memory_dir.display(),
    )
}

fn suggestions(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("SUGGEST_DELETE:").map(str::trim))
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn usage_total(usage: &crate::services::api::claude::NonNullableUsage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.cache_read_input_tokens)
}

/// Paths touched by Write/Edit tool uses in one assistant message.
fn touched_paths(message: &Message) -> Vec<String> {
    let Message::Assistant(assistant) = message else {
        return Vec::new();
    };
    assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::ToolUse(tool_use)
                if matches!(tool_use.name.as_str(), "Write" | "Edit") =>
            {
                tool_use
                    .input
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }
            _ => None,
        })
        .collect()
}

/// Run one consolidation pass. The lock is rolled back on every non-success
/// path, including timeout and abort, so a later idle pass may retry.
///
/// `store` (when given) registers a Dream background task so the pass shows in
/// the task list and `kill_dream_task` can stop it; `None` runs headless (the
/// `/memory` manual path has no task row).
pub async fn run_consolidation(
    context: ToolUseContext,
    cache_safe_params: Arc<CacheSafeParams>,
    max_turns: u32,
    timeout: Duration,
    store: Option<crate::state::store::AppStore>,
) -> Result<ConsolidationResult, ConsolidationError> {
    run_consolidation_with_deps(
        context,
        cache_safe_params,
        max_turns,
        timeout,
        store,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn run_consolidation_with_deps<D: crate::query::deps::QueryDeps>(
    context: ToolUseContext,
    cache_safe_params: Arc<CacheSafeParams>,
    max_turns: u32,
    timeout: Duration,
    store: Option<crate::state::store::AppStore>,
    deps: D,
) -> Result<ConsolidationResult, ConsolidationError> {
    // Serialized with extraction: both rewrite MEMORY.md and the index policy
    // check compares against a pre-run snapshot.
    let Some(_agent_gate) = crate::services::extract_memories::driver::try_memory_agent_gate()
    else {
        return Err(ConsolidationError::Busy(
            "a memory extraction is running".to_string(),
        ));
    };
    let memory_dir = crate::memdir::paths::get_auto_mem_path();
    let mut lock = ConsolidationLock::acquire(&memory_dir).map_err(|error| match error {
        LockAcquireError::Busy { pid, started_at_ms } => {
            ConsolidationError::Busy(format!("pid {pid}, started at {started_at_ms}"))
        }
        LockAcquireError::Io(error) => ConsolidationError::Lock(error.to_string()),
    })?;
    let index_path = crate::memdir::paths::get_auto_mem_entrypoint();
    let index = std::fs::read_to_string(&index_path).unwrap_or_default();
    if !crate::memdir::write_guard::check_memory_injection(&index_path, &index) {
        let _ = lock.rollback();
        return Err(ConsolidationError::MemoryGuardBlocked);
    }
    let candidates = crate::memdir::memdir::list_memory_candidates();
    let prompt = consolidation_prompt(&index, &candidates, &memory_dir);

    // @nyan: own child controller so timeout/kill stop only this fork.
    let fork_abort = AbortController::child_of(context.abort_controller.clone());
    // @nyan: CC's `sessionsReviewing` counts prior session transcripts; this
    // consolidation reviews the project's memory files instead, so the pill
    // reports that scope (the label is CC parity).
    let reviewed_files = memory_activity_since(&memory_dir, 0).1;
    let task_id = store.as_ref().map(|store| {
        crate::tasks::dream_task::register_dream_task(
            store,
            reviewed_files,
            lock.prior_mtime_ms().unwrap_or(0),
            fork_abort.clone(),
            Some(crate::tasks::dream_task::DreamLockLease {
                memory_dir: memory_dir.clone(),
                prior_mtime_ms: lock.prior_mtime_ms(),
                start_mtime_ms: lock.start_mtime_ms(),
            }),
        )
    });
    let on_message = store.clone().zip(task_id.clone()).map(|(store, task_id)| {
        Arc::new(move |message: &Message| {
            let Message::Assistant(assistant) = message else {
                return;
            };
            let text =
                crate::utils::messages::get_assistant_message_text(message).unwrap_or_default();
            let tool_use_count = assistant
                .content
                .iter()
                .filter(|block| matches!(block, AssistantContent::ToolUse(_)))
                .count();
            crate::tasks::dream_task::add_dream_turn(
                &task_id,
                crate::tasks::dream_task::DreamTurn {
                    text,
                    tool_use_count,
                },
                &touched_paths(message),
                &store,
            );
        }) as Arc<dyn Fn(&Message) + Send + Sync>
    });
    let settle = |outcome: &Result<ConsolidationResult, ConsolidationError>| {
        if let (Some(store), Some(task_id)) = (store.as_ref(), task_id.as_deref()) {
            match outcome {
                Ok(_) => crate::tasks::dream_task::complete_dream_task(task_id, store),
                // Already `killed` tasks are terminal; fail_dream_task only
                // moves a still-running one.
                Err(_) => crate::tasks::dream_task::fail_dream_task(task_id, store),
            }
        }
    };
    let params = ForkedAgentParams {
        prompt_messages: vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(prompt)],
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
        cache_safe_params: (*cache_safe_params).clone(),
        can_use_tool: crate::services::extract_memories::memory_tool_callback(
            &context,
            &memory_dir,
        ),
        query_source: QuerySource::AutoDream,
        fork_label: "auto-dream".to_string(),
        overrides: Some(SubagentContextOverrides {
            abort_controller: Some(fork_abort.clone()),
            ..Default::default()
        }),
        max_output_tokens: None,
        max_turns: Some(max_turns.max(1)),
        on_message,
        on_progress: None,
        skip_cache_write: true,
        skip_transcript: true,
    };
    let outcome = run_fork(params, timeout, &fork_abort, &index, &mut lock, deps).await;
    settle(&outcome);
    outcome
}

async fn run_fork<D: crate::query::deps::QueryDeps>(
    params: ForkedAgentParams,
    timeout: Duration,
    fork_abort: &AbortController,
    index_before: &str,
    lock: &mut ConsolidationLock,
    deps: D,
) -> Result<ConsolidationResult, ConsolidationError> {
    let result = match tokio::time::timeout(
        timeout,
        crate::utils::forked_agent::run_forked_agent_with_deps(params, deps),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let _ = crate::memdir::memdir::enforce_agent_index_policy(index_before);
            let _ = lock.rollback();
            if crate::utils::errors::is_abort_error(&error) {
                return Err(ConsolidationError::Aborted);
            }
            return Err(ConsolidationError::Fork(error.to_string()));
        }
        Err(_) => {
            fork_abort.abort();
            let _ = crate::memdir::memdir::enforce_agent_index_policy(index_before);
            let _ = lock.rollback();
            return Err(ConsolidationError::TimedOut);
        }
    };
    let usage = result.total_usage;
    // `run_forked_agent` returns Ok when the query was aborted (kill or
    // session abort); that is not a completed consolidation.
    if fork_abort.is_aborted() {
        let _ = crate::memdir::memdir::enforce_agent_index_policy(index_before);
        let _ = lock.rollback();
        return Err(ConsolidationError::Aborted);
    }
    if let Err(error) = crate::memdir::memdir::enforce_agent_index_policy(index_before) {
        let _ = lock.rollback();
        return Err(ConsolidationError::Fork(format!(
            "consolidation index policy violation: {error}"
        )));
    }
    if !result.api_errors.is_empty() {
        let _ = lock.rollback();
        return Err(ConsolidationError::Fork(
            "consolidation fork returned API errors".to_string(),
        ));
    }
    let text = crate::utils::forked_agent::extract_result_text(&result.messages, "");
    lock.complete()
        .map_err(|error| ConsolidationError::Lock(error.to_string()))?;
    Ok(ConsolidationResult {
        suggested_deletions: suggestions(&text),
        input_tokens: usage_total(&usage),
        output_tokens: usage.output_tokens,
    })
}

/// `/memory` → "Consolidate now". Bypasses every threshold and the Decision
/// Provider (manual runs never consult it), but still takes the lock, uses the
/// restricted tool set and registers a Dream task when a store is available.
///
/// Returns a user-facing summary; every failure is a named error, never an
/// empty success.
pub async fn run_manual_consolidation(
    store: Option<crate::state::store::AppStore>,
) -> Result<String, String> {
    if !crate::memdir::paths::is_auto_memory_enabled() {
        return Err("Auto-memory is off; nothing to consolidate.".to_string());
    }
    // `consolidate: "off"` disables consolidation entirely; `"manual"` is
    // exactly this path (the automatic trigger checks for `"auto"`).
    let mode = crate::utils::settings::get_initial_settings()
        .nyan
        .as_ref()
        .map(|nyan| nyan.memory().consolidate())
        .unwrap_or(crate::utils::settings::nyan::ConsolidateMode::Auto);
    if mode == crate::utils::settings::nyan::ConsolidateMode::Off {
        return Err(
            "Memory consolidation is off (settings.nyan.memory.consolidate = \"off\").".to_string(),
        );
    }
    let Some(cache_safe_params) = crate::utils::forked_agent::get_last_cache_safe_params() else {
        return Err(
            "Consolidation needs a finished turn in this session first (no cache-safe parameters yet)."
                .to_string(),
        );
    };
    let context = cache_safe_params.tool_use_context.clone();
    match run_consolidation(
        context,
        cache_safe_params,
        CONSOLIDATION_MAX_TURNS,
        Duration::from_secs(10 * 60),
        store,
    )
    .await
    {
        Ok(result) => {
            let mut text = format!(
                "Memory consolidation finished ({} input / {} output tokens).",
                result.input_tokens, result.output_tokens
            );
            if !result.suggested_deletions.is_empty() {
                text.push_str("\nSuggested deletions (not deleted; confirm in /memory):");
                for path in &result.suggested_deletions {
                    text.push_str(&format!("\n  {path}"));
                }
            }
            Ok(text)
        }
        Err(ConsolidationError::Busy(who)) => {
            Err(format!("Consolidation is already running ({who})."))
        }
        Err(ConsolidationError::TimedOut) => {
            Err("Consolidation timed out; the lock was rolled back.".to_string())
        }
        Err(ConsolidationError::Aborted) => {
            Err("Consolidation was cancelled; the lock was rolled back.".to_string())
        }
        Err(ConsolidationError::MemoryGuardBlocked) => Err(
            "MEMORY.md was isolated by the secret guard; consolidation did not run.".to_string(),
        ),
        Err(ConsolidationError::Lock(error)) => Err(format!("Consolidation lock error: {error}")),
        Err(ConsolidationError::Fork(error)) => Err(format!("Consolidation failed: {error}")),
    }
}

/// @nyan: consolidation reads and rewrites many memory files, so it gets a
/// more generous turn budget than extraction (`extractMaxTurns` is a
/// per-extraction knob).
const CONSOLIDATION_MAX_TURNS: u32 = 12;

/// Minimum spacing between automatic attempts in one process. A failed pass
/// rolls the lock back (so the next idle may retry), which without this would
/// start a new fork on every turn end.
const AUTO_RETRY_BACKOFF: Duration = Duration::from_secs(10 * 60);

static LAST_AUTO_ATTEMPT: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

fn mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
}

/// (newest mtime, count) of topic files strictly newer than `since_ms`.
fn memory_activity_since(memory_dir: &Path, since_ms: u64) -> (Option<u64>, usize) {
    fn walk(dir: &Path, since_ms: u64, newest: &mut Option<u64>, count: &mut usize) {
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
                    walk(&path, since_ms, newest, count);
                }
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("md")
                && path.file_name().and_then(|name| name.to_str()) != Some("MEMORY.md")
                && let Some(mtime) = mtime_ms(&path).filter(|mtime| *mtime > since_ms)
            {
                *count += 1;
                *newest = Some(newest.map_or(mtime, |current| current.max(mtime)));
            }
        }
    }
    let (mut newest, mut count) = (None, 0);
    walk(memory_dir, since_ms, &mut newest, &mut count);
    (newest, count)
}

/// Idle trigger, called at turn end on the main thread. Returns immediately;
/// the pass runs detached on the process runtime and shows in the task list.
///
/// @nyan: `autoDreamEnabled` is the user-facing switch (the `/memory` toggle,
/// previously a dead setting); `nyan.memory.consolidate` must also be `auto`.
pub fn spawn_idle_consolidation_if_due(
    context: &ToolUseContext,
    cache_safe_params: Option<Arc<CacheSafeParams>>,
    query_source: &QuerySource,
) {
    use crate::utils::settings::nyan::ConsolidateMode;
    if !matches!(query_source, QuerySource::Prompt)
        || context.agent_id.is_some()
        || !crate::memdir::paths::is_auto_memory_enabled()
        || crate::utils::env_utils::is_bare_mode()
    {
        return;
    }
    let settings = crate::utils::settings::get_initial_settings();
    let memory = settings
        .nyan
        .as_ref()
        .map(|nyan| nyan.memory())
        .unwrap_or_default();
    if !settings.auto_dream_enabled.unwrap_or(false)
        || memory.consolidate() != ConsolidateMode::Auto
    {
        return;
    }
    let Some(cache_safe_params) = cache_safe_params else {
        return;
    };
    let memory_dir = crate::memdir::paths::get_auto_mem_path();
    let last_start_ms = mtime_ms(&lock::ConsolidationLock::path(&memory_dir));
    let (newest, count) = memory_activity_since(&memory_dir, last_start_ms.unwrap_or(0));
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    let due_now = consolidation_is_due(
        last_start_ms,
        newest,
        count,
        memory.consolidate_min_hours(),
        memory.consolidate_min_new_files(),
        now_ms,
    );
    // @nyan (C4): `memory.consolidate` shadow comparison. Detached and
    // side-effect free; `due_now` below still decides whether a pass starts.
    if crate::services::memory_decision::shadow_enabled_now(
        crate::services::decision::DecisionPoint::MemoryConsolidate,
    ) {
        let shadow = crate::services::memory_decision::ConsolidationContext {
            last_start_ms,
            newest_file_ms: newest,
            new_file_count: count,
            min_hours: memory.consolidate_min_hours(),
            min_new_files: memory.consolidate_min_new_files(),
            now_ms,
            deterministic_due: due_now,
        };
        crate::services::memory_decision::spawn_detached(async move {
            let _ = crate::services::memory_decision::evaluate_consolidate(shadow).await;
        });
    }
    if !due_now || crate::services::extract_memories::driver::memory_agent_busy() {
        // Never record an attempt for a pass that could not start: the idle
        // trigger is cheap to re-evaluate next turn.
        return;
    }
    {
        let mut last = LAST_AUTO_ATTEMPT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.is_some_and(|at| at.elapsed() < AUTO_RETRY_BACKOFF) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() else {
        tracing::warn!(
            target: "nyan::memory",
            "auto_dream_skipped: no process runtime available for detached work"
        );
        return;
    };
    let context = context.clone();
    let store = context
        .app_store
        .tasks_store
        .clone()
        .or_else(|| context.app_store.store.clone());
    let max_turns = CONSOLIDATION_MAX_TURNS;
    handle.spawn(async move {
        match run_consolidation(
            context,
            cache_safe_params,
            max_turns,
            Duration::from_secs(10 * 60),
            store,
        )
        .await
        {
            Ok(result) => tracing::info!(
                target: "nyan::memory",
                suggested_deletions = result.suggested_deletions.len(),
                "auto_dream_ok"
            ),
            Err(error) => {
                tracing::warn!(target: "nyan::memory", ?error, "auto_dream_failed")
            }
        }
    });
}

/// Deterministic automatic-trigger predicate. Manual callers bypass this.
pub fn consolidation_is_due(
    last_start_ms: Option<u64>,
    newest_file_ms: Option<u64>,
    new_file_count: usize,
    min_hours: u32,
    min_new_files: u32,
    now_ms: u64,
) -> bool {
    let elapsed_ok = last_start_ms
        .map(|last| now_ms.saturating_sub(last) >= u64::from(min_hours) * 3_600_000)
        .unwrap_or(true);
    elapsed_ok
        && new_file_count >= min_new_files as usize
        && newest_file_ms.is_some_and(|mtime| mtime > last_start_ms.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// One scripted model turn; `before_answer` runs while the "fork" is live
    /// so tests can simulate index writes or external kills.
    #[derive(Clone)]
    struct ScriptedDeps {
        text: String,
        api_error: bool,
        before_answer: Arc<dyn Fn() + Send + Sync>,
    }

    impl crate::query::deps::QueryDeps for ScriptedDeps {
        fn call_model(
            &self,
            _request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let text = self.text.clone();
            let api_error = self.api_error;
            let before_answer = self.before_answer.clone();
            Box::pin(async move {
                (before_answer)();
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                tx.send(crate::services::api::claude::QueryModelStreamItem::Stream(
                    crate::types::message::StreamEvent::ApiEvent {
                        event: serde_json::json!({
                            "type": "message_delta",
                            "usage": {"input_tokens": 9, "output_tokens": 4}
                        }),
                        ttft_ms: Some(1),
                    },
                ))
                .await
                .unwrap();
                if api_error {
                    tx.send(
                        crate::services::api::claude::QueryModelStreamItem::SystemError(
                            crate::types::message::SystemApiErrorMessage {
                                content: "boom".to_string(),
                                api_error: "boom".to_string(),
                                error: "error".to_string(),
                                error_details: None,
                            },
                        ),
                    )
                    .await
                    .unwrap();
                }
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        crate::types::message::AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![crate::types::message::AssistantContent::Text(text)],
                            model: Some("m".to_string()),
                            stop_reason: Some(crate::types::message::StopReason::EndTurn),
                            usage: None,
                        },
                    ),
                )
                .await
                .unwrap();
                Ok(rx)
            })
        }
    }

    struct Fixture {
        root: std::path::PathBuf,
        _lock: crate::utils::test_env::TestEnvGuard<'static>,
        _env: EnvVarGuard,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture() -> Fixture {
        let lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("nyan-dream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let env = EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root);
        Fixture {
            root,
            _lock: lock,
            _env: env,
        }
    }

    fn cache(context: &ToolUseContext) -> Arc<CacheSafeParams> {
        Arc::new(CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        })
    }

    fn lock_path(root: &Path) -> std::path::PathBuf {
        lock::ConsolidationLock::path(root)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn completed_pass_settles_the_lock_and_parses_delete_suggestions() {
        let fixture = fixture();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let result = run_consolidation_with_deps(
            context.clone(),
            cache(&context),
            4,
            Duration::from_secs(20),
            None,
            ScriptedDeps {
                text: "SUGGEST_DELETE: /memory/old.md\nDone.".to_string(),
                api_error: false,
                before_answer: Arc::new(|| {}),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.suggested_deletions, vec!["/memory/old.md"]);
        assert_eq!(result.output_tokens, 4);
        // Complete keeps the start mtime as "last ran" and clears the pid.
        assert_eq!(
            std::fs::read_to_string(lock_path(&fixture.root)).unwrap(),
            ""
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn api_errors_roll_the_lock_back_so_the_next_idle_can_retry() {
        let fixture = fixture();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let result = run_consolidation_with_deps(
            context.clone(),
            cache(&context),
            4,
            Duration::from_secs(20),
            None,
            ScriptedDeps {
                text: "partial".to_string(),
                api_error: true,
                before_answer: Arc::new(|| {}),
            },
        )
        .await;
        assert!(matches!(result, Err(ConsolidationError::Fork(_))));
        assert!(
            !lock_path(&fixture.root).exists(),
            "a failed pass with no prior lock removes it"
        );
        assert!(lock::ConsolidationLock::acquire(&fixture.root).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn store_registers_a_dream_task_that_completes_and_reports_turns() {
        let _fixture = fixture();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = ToolUseContext::default().with_main_loop_model("m");
        run_consolidation_with_deps(
            context.clone(),
            cache(&context),
            4,
            Duration::from_secs(20),
            Some(store.clone()),
            ScriptedDeps {
                text: "merged".to_string(),
                api_error: false,
                before_answer: Arc::new(|| {}),
            },
        )
        .await
        .unwrap();
        let state = store.get();
        let (_, task) = state
            .tasks
            .iter()
            .find(|(_, task)| {
                matches!(
                    task.as_ref(),
                    crate::state::app_state_store::TaskState::Dream(_)
                )
            })
            .unwrap();
        let crate::state::app_state_store::TaskState::Dream(task) = task.as_ref() else {
            unreachable!()
        };
        assert_eq!(task.status, "completed");
        assert_eq!(task.turns.last().unwrap().text, "merged");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn indexing_a_user_memory_during_consolidation_is_rolled_back() {
        let fixture = fixture();
        let index = crate::memdir::paths::get_auto_mem_entrypoint();
        std::fs::write(&index, "- [kept](kept.md) — kept\n").unwrap();
        let root = fixture.root.clone();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let result = run_consolidation_with_deps(
            context.clone(),
            cache(&context),
            4,
            Duration::from_secs(20),
            None,
            ScriptedDeps {
                text: "done".to_string(),
                api_error: false,
                before_answer: Arc::new(move || {
                    let index = root.join("MEMORY.md");
                    let mut text = std::fs::read_to_string(&index).unwrap();
                    text.push_str("- [me](me.md) — about me\n");
                    std::fs::write(&index, text).unwrap();
                    std::fs::write(
                        root.join("me.md"),
                        "---\nname: me\ndescription: d\ntype: user\n---\nbody\n",
                    )
                    .unwrap();
                }),
            },
        )
        .await;
        assert!(matches!(result, Err(ConsolidationError::Fork(_))));
        assert_eq!(
            std::fs::read_to_string(&index).unwrap(),
            "- [kept](kept.md) — kept\n"
        );
        assert!(!lock_path(&fixture.root).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn consolidation_refuses_to_run_while_extraction_holds_the_gate() {
        let _fixture = fixture();
        let _gate = crate::services::extract_memories::driver::test_gate_guard();
        let context = ToolUseContext::default().with_main_loop_model("m");
        let result = run_consolidation_with_deps(
            context.clone(),
            cache(&context),
            4,
            Duration::from_secs(20),
            None,
            ScriptedDeps {
                text: "done".to_string(),
                api_error: false,
                before_answer: Arc::new(|| {}),
            },
        )
        .await;
        assert!(matches!(result, Err(ConsolidationError::Busy(_))));
        assert!(
            !lock_path(&_fixture.root).exists(),
            "a refused pass must not even take the lock"
        );
    }

    #[test]
    fn memory_activity_counts_only_topic_files_newer_than_the_marker() {
        let root = std::env::temp_dir().join(format!("nyan-activity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::write(root.join("MEMORY.md"), "index").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join("nested.md"), "b").unwrap();
        std::fs::write(root.join("logs").join("day.md"), "log").unwrap();
        std::fs::write(root.join("ignore.txt"), "x").unwrap();
        // Everything is newer than 0; MEMORY.md, logs/ and non-.md do not count.
        let (_, count) = memory_activity_since(&root, 0);
        assert_eq!(count, 2);
        // Nothing is newer than "now + 1s".
        let now = mtime_ms(&root.join("a.md")).unwrap() + 1_000;
        let (newest, count) = memory_activity_since(&root, now);
        assert_eq!((newest, count), (None, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn automatic_trigger_requires_both_time_and_new_file_thresholds() {
        assert!(!consolidation_is_due(
            Some(1_000),
            Some(2_000),
            4,
            1,
            5,
            3_601_000
        ));
        assert!(consolidation_is_due(
            Some(1_000),
            Some(2_000),
            5,
            1,
            5,
            3_601_000
        ));
        assert!(!consolidation_is_due(
            Some(3_000),
            Some(2_000),
            5,
            1,
            5,
            3_601_000
        ));
    }
}
